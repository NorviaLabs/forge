//! Sidebar panels (TUI-03 / tui-sidebar.md).

use crate::theme;
use forge_core::AgentSession;
use forge_types::SessionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

#[derive(Debug, Clone)]
pub struct SidebarModel {
    pub session_id: String,
    pub status: String,
    pub surface: String,
    pub role: String,
    pub ctx_pct: f64,
    pub ctx_used: usize,
    pub ctx_total: usize,
    pub tools: Vec<String>,
    pub activity: Vec<String>,
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
            SessionStatus::AwaitingHitl => "awaiting_hitl",
        };
        let mut tools = session.list_tools();
        tools.sort();
        let context = session.token_usage_report();
        Self {
            session_id: short.to_string(),
            status: status.into(),
            surface: "tui".into(),
            role: "generator".into(),
            ctx_pct: session.context_usage_ratio(),
            ctx_used: context.context_tokens_est,
            ctx_total: context.context_capacity,
            tools,
            activity: activity_lines.to_vec(),
        }
    }
}

#[allow(dead_code)]
pub struct SidebarWidget<'a> {
    pub model: &'a SidebarModel,
}

impl Widget for SidebarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .style(theme::panel_alt())
            .title(Span::styled(" session ", theme::brand()));
        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7),
                Constraint::Length(6),
                Constraint::Min(4),
            ])
            .split(inner);

        // Session
        let sess_lines = vec![
            Line::from(Span::styled("SESSION", theme::dim())),
            Line::from(vec![
                Span::styled("id      ", theme::dim()),
                Span::styled(self.model.session_id.clone(), theme::text()),
            ]),
            Line::from(vec![
                Span::styled("status  ", theme::dim()),
                Span::styled(self.model.status.clone(), status_style(&self.model.status)),
            ]),
            Line::from(vec![
                Span::styled("surface ", theme::dim()),
                Span::styled(self.model.surface.clone(), theme::text()),
            ]),
            Line::from(vec![
                Span::styled("role    ", theme::dim()),
                Span::styled(self.model.role.clone(), theme::text()),
            ]),
        ];
        Paragraph::new(sess_lines).render(chunks[0], buf);

        let pct = (self.model.ctx_pct * 100.0).clamp(0.0, 100.0);
        let tool_lines = vec![
            Line::from(Span::styled("CONTEXT BUDGET", theme::dim())),
            Line::from(vec![
                Span::styled("used ", theme::dim()),
                Span::styled(format!("{pct:.0}%"), theme::info()),
            ]),
            Line::from(Span::styled(
                format!(
                    "{}k / {}k tokens",
                    self.model.ctx_used / 1000,
                    self.model.ctx_total / 1000
                ),
                theme::muted(),
            )),
            Line::from(""),
            Line::from(Span::styled("TOOLS (ACL)", theme::dim())),
            Line::from(vec![
                Span::styled("built-ins ", theme::dim()),
                Span::styled(
                    format!("{} allowed", self.model.tools.len()),
                    theme::muted(),
                ),
            ]),
        ];

        Paragraph::new(tool_lines).render(chunks[1], buf);

        // Activity is intentionally compact; detailed output belongs in chat.
        let heading = if self.model.activity.is_empty() {
            "RECENT JOURNAL"
        } else {
            "ACTIVITY"
        };
        let mut activity_lines = vec![Line::from(Span::styled(heading, theme::dim()))];
        if self.model.activity.is_empty() {
            activity_lines.push(Line::from(Span::styled("—", theme::dim())));
        } else {
            let max = (chunks[2].height as usize).saturating_sub(1).max(1);
            for summary in self.model.activity.iter().rev().take(max) {
                let text: String = summary.chars().take(34).collect();
                activity_lines.push(Line::from(Span::styled(
                    format!("· {text}"),
                    theme::muted(),
                )));
            }
        }
        Paragraph::new(activity_lines).render(chunks[2], buf);
    }
}

#[allow(dead_code)]
fn status_style(s: &str) -> ratatui::style::Style {
    match s {
        "awaiting_hitl" => theme::warn(),
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

        let m = SidebarModel::from_session_with_activity(&s, &["model started".into()]);
        assert_eq!(m.activity, vec!["model started"]);
    }
}
