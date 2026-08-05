//! Contextual hint slot for the footer.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;

#[derive(Debug, Clone)]
pub struct FooterModel {
    pub cwd: String,
    pub session_short: String,
    pub status: String,
    pub status_busy: bool,
    /// Active vendor label (e.g. "OpenAI"), or empty when unknown.
    pub provider: String,
    /// Wire model id (may be `provider/model`); footer shows the short id.
    pub model: String,
    pub effort: String,
    pub ctx_used: usize,
    pub ctx_total: usize,
    pub ctx_pct: f64,
    pub connected: bool,
    pub connect_profile: Option<String>,
    /// Extra shortcut strip (busy / idle).
    pub hints: String,
    pub usage_summary: String,
    pub usage: String,
    pub weekly_limit: String,
    pub credits: String,
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
            hints: String::new(),
            usage_summary: String::new(),
            usage: String::new(),
            weekly_limit: String::new(),
            credits: String::new(),
        }
    }
}

pub struct FooterBar<'a> {
    pub model: &'a FooterModel,
}

/// Strip a `provider/` prefix from a wire model id for display.
pub fn footer_short_model_id(model: &str) -> &str {
    match model.split_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => model,
    }
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        // Steady-state vendor/model/effort moved to the composer chip row.
        // Footer is contextual navigation hints only.
        if !self.model.hints.is_empty() {
            buf.set_stringn(
                area.x,
                area.y,
                self.model.hints.as_str(),
                area.width as usize,
                theme::muted(),
            );
        }
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
            usage: String::new(),
            weekly_limit: String::new(),
            credits: String::new(),
            ctx_used: 10,
            ctx_total: 100,
            ctx_pct: 0.1,
            connected: true,
            connect_profile: Some("xai".into()),
            hints: "test".into(),
            usage_summary: "in 7 · out 5 · tok 12".into(),
        };
        assert!(m.cwd.contains("tmp"));
    }

    #[test]
    fn renders_contextual_hint_only() {
        let model = FooterModel {
            status: "thinking".into(),
            status_busy: true,
            provider: "native".into(),
            model: "gpt".into(),
            hints: "Enter confirm · Esc cancel".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Enter confirm"));
        assert!(!rendered.contains("thinking"));
        assert!(!rendered.contains("native"));
        assert!(!rendered.contains("gpt"));
    }

    #[test]
    fn renders_nothing_when_connected_and_no_hint() {
        // Compact vendor/model/effort moved to the composer chip row.
        let model = FooterModel {
            connected: true,
            provider: "Anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            effort: "Low".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.trim().is_empty(), "{rendered:?}");
    }

    #[test]
    fn renders_nothing_when_disconnected_and_no_hint() {
        let model = FooterModel {
            connected: false,
            provider: "Anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            effort: "Low".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.trim().is_empty(), "{rendered:?}");
    }

    #[test]
    fn hint_takes_priority_over_the_compact_control() {
        let model = FooterModel {
            hints: "Enter confirm · Esc cancel".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Enter confirm"));
        assert!(!rendered.contains("claude-sonnet-4-6"));
    }
}
