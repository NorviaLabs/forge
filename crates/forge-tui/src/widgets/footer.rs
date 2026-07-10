//! Footer bar — keyboard hints (mode-aware).

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

#[derive(Debug, Clone)]
pub struct FooterModel {
    pub version: String,
    pub cwd: String,
    pub provider: String,
    /// Extra shortcut strip (busy / idle).
    pub hints: String,
}

impl Default for FooterModel {
    fn default() -> Self {
        Self {
            version: String::new(),
            cwd: String::new(),
            provider: String::new(),
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
        let line = Line::from(vec![
            Span::styled(format!(" {} ", self.model.version), theme::dim()),
            Span::styled(format!("{cwd} "), theme::dim()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("{} ", self.model.hints), theme::muted()),
        ]);
        buf.set_line(area.x, area.y, &line, area.width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footer_model_holds_fields() {
        let m = FooterModel {
            version: "0.4.0".into(),
            cwd: "/tmp".into(),
            provider: "mock".into(),
            hints: "test".into(),
        };
        assert!(m.version.contains('0'));
    }
}
