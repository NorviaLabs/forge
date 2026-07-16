//! Footer bar — keyboard hints (mode-aware).

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

#[derive(Debug, Clone)]
pub struct FooterModel {
    pub cwd: String,
    pub session_short: String,
    pub status: String,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub ctx_used: usize,
    pub ctx_total: usize,
    pub ctx_pct: f64,
    pub connected: bool,
    pub connect_profile: Option<String>,
    pub worktree_on: bool,
    /// Extra shortcut strip (busy / idle).
    pub hints: String,
}

impl Default for FooterModel {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            session_short: String::new(),
            status: String::new(),
            provider: String::new(),
            model: String::new(),
            effort: String::new(),
            ctx_used: 0,
            ctx_total: 0,
            ctx_pct: 0.0,
            connected: false,
            connect_profile: None,
            worktree_on: false,
            hints: "Enter send · ⇧Enter newline · Ctrl+K cmds · Esc clear".into(),
        }
    }
}

pub struct FooterBar<'a> {
    pub model: &'a FooterModel,
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let cwd = if self.model.cwd.chars().count() > 28 {
            let s: String = self.model.cwd.chars().rev().take(26).collect();
            format!("…{}", s.chars().rev().collect::<String>())
        } else {
            self.model.cwd.clone()
        };
        let conn = if self.model.connected {
            let who = self.model.connect_profile.as_deref().unwrap_or("live");
            format!("connected:{who}")
        } else {
            "not connected".into()
        };
        let model_disp = if self.model.provider.is_empty() {
            self.model.model.clone()
        } else if self.model.model.is_empty() {
            self.model.provider.clone()
        } else {
            format!("{}/{}", self.model.provider, self.model.model)
        };
        let pct = (self.model.ctx_pct * 100.0).clamp(0.0, 100.0);
        let mut spans = vec![
            Span::styled(format!("{cwd} "), theme::dim()),
            Span::styled("· ", theme::dim()),
            Span::styled(
                format!("session={} ", self.model.session_short),
                theme::muted(),
            ),
            Span::styled("· ", theme::dim()),
            Span::styled(
                format!("{} ", conn),
                if self.model.connected {
                    theme::ok()
                } else {
                    theme::warn()
                },
            ),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("{} ", self.model.status), theme::text()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("{} ", model_disp), theme::text()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("effort={} ", self.model.effort), theme::text()),
            Span::styled("· ", theme::dim()),
            Span::styled(
                format!(
                    "ctx {}/{} [{pct:.1}%] ",
                    self.model.ctx_used, self.model.ctx_total
                ),
                theme::info(),
            ),
        ];
        if self.model.worktree_on {
            spans.push(Span::styled("· ", theme::dim()));
            spans.push(Span::styled("worktree ", theme::warn()));
        }
        spans.push(Span::styled("· ", theme::dim()));
        spans.push(Span::styled(
            format!("{} ", self.model.hints),
            theme::muted(),
        ));
        let line = Line::from(spans);
        buf.set_line(area.x, area.y, &line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_model_holds_fields() {
        let m = FooterModel {
            cwd: "/tmp".into(),
            session_short: "abcd".into(),
            status: "idle".into(),
            provider: "mock".into(),
            model: "m".into(),
            effort: "auto".into(),
            ctx_used: 10,
            ctx_total: 100,
            ctx_pct: 0.1,
            connected: true,
            connect_profile: Some("xai".into()),
            worktree_on: false,
            hints: "test".into(),
        };
        assert!(m.cwd.contains("tmp"));
    }
}
