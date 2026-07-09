//! Sidebar panels (TUI-03 / tui-sidebar.md).

use crate::theme;
use forge_core::AgentSession;
use forge_types::SessionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Widget};

#[derive(Debug, Clone)]
pub struct SidebarModel {
    pub session_id: String,
    pub status: String,
    pub surface: String,
    pub role: String,
    pub ctx_pct: f64,
    pub tools_allowed: usize,
    pub tools_total_hint: String,
    /// Phase 10 — activity feed lines (preferred over raw events when set).
    pub activity: Vec<String>,
    pub events: Vec<String>,
    pub worktree: Option<String>,
}

impl SidebarModel {
    pub fn from_session(session: &AgentSession) -> Self {
        Self::from_session_with_activity(session, &[])
    }

    pub fn from_session_with_activity(
        session: &AgentSession,
        activity_lines: &[String],
    ) -> Self {
        let id = session.session_id.to_string();
        let short = if id.len() > 8 { &id[..8] } else { &id };
        let status = match session.status {
            SessionStatus::Running => "running",
            SessionStatus::Completed => "completed",
            SessionStatus::Failed => "failed",
            SessionStatus::AwaitingHitl => "awaiting_hitl",
        };
        let tools = session.list_tools();
        let events: Vec<String> = session
            .events
            .iter()
            .rev()
            .take(8)
            .map(|e| {
                let d: String = e.detail.chars().take(36).collect();
                format!("{} {}", e.kind, d)
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        Self {
            session_id: short.to_string(),
            status: status.into(),
            surface: "tui".into(),
            role: "generator".into(),
            ctx_pct: session.context_usage_ratio(),
            tools_allowed: tools.len(),
            tools_total_hint: format!("{} visible", tools.len()),
            activity: activity_lines.to_vec(),
            events,
            worktree: session.worktree_status(),
        }
    }
}

pub struct SidebarWidget<'a> {
    pub model: &'a SidebarModel,
}

impl Widget for SidebarWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(theme::border())
            .title(Span::styled(" session ", theme::muted()));
        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(6),
                Constraint::Length(4),
                Constraint::Length(4),
                Constraint::Min(3),
            ])
            .split(inner);

        // Session
        let sess_lines = vec![
            Line::from(Span::styled("SESSION", theme::dim())),
            Line::from(vec![
                Span::styled("id ", theme::muted()),
                Span::styled(self.model.session_id.clone(), theme::text()),
            ]),
            Line::from(vec![
                Span::styled("status ", theme::muted()),
                Span::styled(self.model.status.clone(), status_style(&self.model.status)),
            ]),
            Line::from(vec![
                Span::styled("surface ", theme::muted()),
                Span::styled(self.model.surface.clone(), theme::text()),
            ]),
            Line::from(vec![
                Span::styled("role ", theme::muted()),
                Span::styled(self.model.role.clone(), theme::text()),
            ]),
        ];
        Paragraph::new(sess_lines).render(chunks[0], buf);

        // Budget
        let pct = (self.model.ctx_pct.clamp(0.0, 1.0) * 100.0) as u16;
        let label = format!("CONTEXT  {pct}%");
        Paragraph::new(Line::from(Span::styled(label, theme::dim()))).render(
            Rect {
                x: chunks[1].x,
                y: chunks[1].y,
                width: chunks[1].width,
                height: 1,
            },
            buf,
        );
        Gauge::default()
            .gauge_style(theme::brand())
            .ratio(self.model.ctx_pct.clamp(0.0, 1.0))
            .render(
                Rect {
                    x: chunks[1].x,
                    y: chunks[1].y + 1,
                    width: chunks[1].width,
                    height: 1,
                },
                buf,
            );

        // Tools
        let tool_lines = vec![
            Line::from(Span::styled("TOOLS (ACL)", theme::dim())),
            Line::from(vec![
                Span::styled("allowed ", theme::muted()),
                Span::styled(self.model.tools_allowed.to_string(), theme::ok()),
            ]),
            Line::from(Span::styled(self.model.tools_total_hint.clone(), theme::muted())),
        ];
        Paragraph::new(tool_lines).render(chunks[2], buf);

        // Activity feed (Phase 10) or legacy events
        let title = if self.model.activity.is_empty() {
            "RECENT EVENTS"
        } else {
            "ACTIVITY"
        };
        let mut ev = vec![Line::from(Span::styled(title, theme::dim()))];
        let lines: &[String] = if !self.model.activity.is_empty() {
            &self.model.activity
        } else {
            &self.model.events
        };
        if lines.is_empty() {
            ev.push(Line::from(Span::styled("—", theme::dim())));
        } else {
            for e in lines {
                let truncated: String = e.chars().take(48).collect();
                ev.push(Line::from(Span::styled(truncated, theme::muted())));
            }
        }
        if let Some(ref wt) = self.model.worktree {
            ev.push(Line::from(Span::styled(
                format!("wt: {wt}"),
                theme::info(),
            )));
        }
        Paragraph::new(ev).render(chunks[3], buf);
    }
}

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
    use forge_workspace::IsolationMode;
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
                isolation: IsolationMode::Off,
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
        assert!(m.tools_allowed >= 1);
        assert!(!m.events.is_empty() || m.status == "completed");
        assert!(m.ctx_pct >= 0.0);
    }
}
