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
    pub busy: bool,
    pub step: String,
    pub context_reset: Option<(f64, f64)>,
    pub skills: Vec<String>,
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
            SessionStatus::AwaitingHitl => "awaiting hitl",
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
            busy: false,
            step: String::new(),
            context_reset: None,
            skills: session.loaded_skill_names(),
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
            .borders(Borders::LEFT)
            .border_style(theme::border())
            .style(theme::panel_alt());
        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Length(4),
                Constraint::Min(4),
            ])
            .split(inner);

        // Session
        let mut sess_lines = vec![
            Line::from(Span::styled("Now", theme::brand())),
            Line::from(vec![
                Span::styled("state ", theme::dim()),
                Span::styled(self.model.status.clone(), status_style(&self.model.status)),
            ]),
        ];
        if self.model.busy {
            sess_lines.push(Line::from(vec![
                Span::styled("active ", theme::dim()),
                Span::styled(self.model.step.clone(), theme::info()),
            ]));
        }
        Paragraph::new(sess_lines).render(chunks[0], buf);

        let pct = (self.model.ctx_pct * 100.0).clamp(0.0, 100.0);
        let mut tool_lines = vec![
            Line::from(Span::styled("Context", theme::metadata_style())),
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
        ];
        if let Some((before, after)) = self.model.context_reset {
            tool_lines.splice(
                1..3,
                [
                    Line::from(vec![
                        Span::styled("before ", theme::dim()),
                        Span::styled(format!("{before:.0}%"), theme::warn()),
                    ]),
                    Line::from(vec![
                        Span::styled("after  ", theme::dim()),
                        Span::styled(format!("{after:.0}%"), theme::ok()),
                    ]),
                ],
            );
        }

        Paragraph::new(tool_lines).render(chunks[1], buf);

        let mut lower = Vec::new();
        if !self.model.skills.is_empty() {
            lower.push(Line::from(Span::styled("Skills", theme::metadata_style())));
            let max = (chunks[2].height as usize / 2).max(1);
            for name in self.model.skills.iter().take(max) {
                lower.push(Line::from(Span::styled(format!("· {name}"), theme::text())));
            }
        }
        if !self.model.activity.is_empty() {
            if !lower.is_empty() {
                lower.push(Line::from(""));
            }
            lower.push(Line::from(Span::styled("Recent", theme::metadata_style())));
            let max = (chunks[2].height as usize)
                .saturating_sub(lower.len() + 1)
                .max(1);
            for summary in self.model.activity.iter().rev().take(max) {
                let text: String = summary.chars().take(34).collect();
                lower.push(Line::from(Span::styled(
                    format!("· {text}"),
                    theme::muted(),
                )));
            }
        }
        Paragraph::new(lower).render(chunks[2], buf);
    }
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
