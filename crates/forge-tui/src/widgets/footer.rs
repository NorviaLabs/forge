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
    pub status_busy: bool,
    pub provider: String,
    pub model: String,
    pub effort: String,
    pub ctx_used: usize,
    pub ctx_total: usize,
    pub ctx_pct: f64,
    pub connected: bool,
    pub connect_profile: Option<String>,
    /// Extra shortcut strip (busy / idle).
    pub hints: String,
}

impl Default for FooterModel {
    fn default() -> Self {
        Self {
            cwd: String::new(),
            session_short: String::new(),
            status: String::new(),
            status_busy: false,
            provider: String::new(),
            model: String::new(),
            effort: String::new(),
            ctx_used: 0,
            ctx_total: 0,
            ctx_pct: 0.0,
            connected: false,
            connect_profile: None,
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
        let metadata_y = area.y + area.height.saturating_sub(1);
        if area.height > 1 {
            buf.set_line(
                area.x,
                area.y,
                &Line::from(Span::styled(self.model.hints.as_str(), theme::muted())),
                area.width,
            );
        }
        let _ = (buf, metadata_y);
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
            status_busy: false,
            provider: "mock".into(),
            model: "m".into(),
            effort: "auto".into(),
            ctx_used: 10,
            ctx_total: 100,
            ctx_pct: 0.1,
            connected: true,
            connect_profile: Some("xai".into()),
            hints: "test".into(),
        };
        assert!(m.cwd.contains("tmp"));
    }

    #[test]
    fn does_not_repeat_product_identity() {
        let model = FooterModel {
            status: "thinking".into(),
            status_busy: true,
            hints: String::new(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.trim().is_empty());
    }

    #[test]
    fn does_not_repeat_status_metadata() {
        let model = FooterModel {
            provider: "native".into(),
            model: "openai-codex/gpt-5.6-sol".into(),
            hints: String::new(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 79, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.trim().is_empty());
    }
}
