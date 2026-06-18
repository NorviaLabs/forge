//! Footer bar (TUI-01).

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
}

pub struct FooterBar<'a> {
    pub model: &'a FooterModel,
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let line = Line::from(vec![
            Span::styled(
                format!(" {} ", self.model.version),
                theme::dim(),
            ),
            Span::styled(
                format!("cwd {} ", self.model.cwd),
                theme::dim(),
            ),
            Span::styled(
                format!("provider {} ", self.model.provider),
                theme::dim(),
            ),
            Span::styled("│ / help · Esc cancel · Ctrl+C quit ", theme::dim()),
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
        };
        assert!(m.version.contains('0'));
    }
}
