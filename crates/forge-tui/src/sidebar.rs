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
    pub tools: Vec<String>,
}

impl SidebarModel {
    pub fn from_session(session: &AgentSession) -> Self {
        Self::from_session_with_activity(session, &[])
    }

    pub fn from_session_with_activity(session: &AgentSession, activity_lines: &[String]) -> Self {
        let _ = activity_lines; // sidebar no longer renders activity, but keep API stable
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
        Self {
            session_id: short.to_string(),
            status: status.into(),
            surface: "tui".into(),
            role: "generator".into(),
            ctx_pct: session.context_usage_ratio(),
            tools,
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
            .constraints([Constraint::Length(6), Constraint::Min(4)])
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

        // Tools list (fits remaining height)
        let mut tool_lines = vec![Line::from(Span::styled("TOOLS", theme::dim()))];
        if self.model.tools.is_empty() {
            tool_lines.push(Line::from(Span::styled("—", theme::dim())));
        } else {
            let max = (chunks[1].height as usize).saturating_sub(1).max(1);
            for t in self.model.tools.iter().take(max) {
                let s: String = t.chars().take(32).collect();
                tool_lines.push(Line::from(Span::styled(format!("• {s}"), theme::muted())));
            }
            if self.model.tools.len() > max {
                tool_lines.push(Line::from(Span::styled(
                    format!("+{} more", self.model.tools.len() - max),
                    theme::dim(),
                )));
            }
        }

        Paragraph::new(tool_lines).render(chunks[1], buf);
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
    }
}
