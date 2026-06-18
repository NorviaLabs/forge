//! Status bar widget (TUI-01).

use crate::theme;
use forge_types::SessionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

#[derive(Debug, Clone)]
pub struct StatusModel {
    pub status: SessionStatus,
    pub session_short: String,
    pub model: String,
    pub ctx_pct: f64,
    pub worktree_on: bool,
    pub busy: bool,
}

impl StatusModel {
    pub fn status_label(&self) -> (&'static str, ratatui::style::Style) {
        if self.busy {
            return ("running", theme::info().add_modifier(Modifier::BOLD));
        }
        match self.status {
            SessionStatus::Running => ("idle", theme::ok()),
            SessionStatus::Completed => ("completed", theme::ok()),
            SessionStatus::Failed => ("failed", theme::danger()),
            SessionStatus::AwaitingHitl => ("awaiting_hitl", theme::warn().add_modifier(Modifier::BOLD)),
        }
    }
}

pub struct StatusBar<'a> {
    pub model: &'a StatusModel,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (label, style) = self.model.status_label();
        let wt = if self.model.worktree_on {
            "worktree on"
        } else {
            "worktree off"
        };
        let line = Line::from(vec![
            Span::styled(" FORGE ", theme::brand()),
            Span::styled("│ ", theme::dim()),
            Span::styled(format!(" {label} "), style),
            Span::styled(" │ ", theme::dim()),
            Span::styled(
                format!("session {}", self.model.session_short),
                theme::info(),
            ),
            Span::styled(" │ ", theme::dim()),
            Span::styled(format!("model {}", self.model.model), theme::text()),
            Span::styled(
                format!("  ctx {:.0}% │ {wt} ", self.model.ctx_pct * 100.0),
                theme::muted(),
            ),
        ]);
        buf.set_line(area.x, area.y, &line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hitl_label() {
        let m = StatusModel {
            status: SessionStatus::AwaitingHitl,
            session_short: "abcd".into(),
            model: "mock".into(),
            ctx_pct: 0.1,
            worktree_on: false,
            busy: false,
        };
        assert_eq!(m.status_label().0, "awaiting_hitl");
    }

    #[test]
    fn busy_overrides_idle() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "x".into(),
            model: "m".into(),
            ctx_pct: 0.0,
            worktree_on: true,
            busy: true,
        };
        assert_eq!(m.status_label().0, "running");
    }
}
