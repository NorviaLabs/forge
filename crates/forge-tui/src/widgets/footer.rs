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
        let cwd = if self.model.cwd.chars().count() > 24 {
            let s: String = self.model.cwd.chars().rev().take(22).collect();
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
        let model_disp = if self.model.model.is_empty() {
            self.model.provider.clone()
        } else {
            self.model
                .model
                .rsplit('/')
                .next()
                .unwrap_or(&self.model.model)
                .to_string()
        };
        let pct = (self.model.ctx_pct * 100.0).clamp(0.0, 100.0);
        let metadata_y = area.y + area.height.saturating_sub(1);
        if area.height > 1 {
            buf.set_line(
                area.x,
                area.y,
                &Line::from(Span::styled(self.model.hints.as_str(), theme::muted())),
                area.width,
            );
        }
        if area.width < 80 {
            let line = Line::from(vec![
                Span::styled(format!("{} ", model_disp), theme::text()),
                Span::styled("· ", theme::dim()),
                Span::styled(format!("ctx {pct:.1}% "), theme::info()),
            ]);
            render_line_with_status(
                Rect {
                    y: metadata_y,
                    height: 1,
                    ..area
                },
                buf,
                line,
                &self.model.status,
                self.model.status_busy,
            );
            return;
        }
        let spans = vec![
            Span::styled(
                format!("forge {}  cwd {cwd} ", env!("CARGO_PKG_VERSION")),
                theme::dim(),
            ),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("provider {} ", self.model.provider), theme::muted()),
        ];
        let _ = (model_disp, conn, pct);
        render_line_with_status(
            Rect {
                y: metadata_y,
                height: 1,
                ..area
            },
            buf,
            Line::from(spans),
            &self.model.status,
            self.model.status_busy,
        );
    }
}

fn render_line_with_status(
    area: Rect,
    buf: &mut Buffer,
    line: Line<'_>,
    status_text: &str,
    status_busy: bool,
) {
    let status = format!("{} ", status_text);
    let status_width = status.chars().count().min(area.width as usize) as u16;
    let status_x = area.x + area.width.saturating_sub(status_width);
    let left_width = status_x.saturating_sub(area.x + 1);

    buf.set_line(area.x, area.y, &line, left_width);
    buf.set_string(
        status_x,
        area.y,
        status,
        if status_busy {
            theme::info()
        } else {
            theme::text()
        },
    );
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
    fn renders_status_at_bottom_right() {
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
        assert!(rendered.ends_with("thinking "));
    }

    #[test]
    fn renders_only_the_model_name() {
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
        assert!(rendered.starts_with("gpt-5.6-sol · ctx"));
        assert!(!rendered.contains("native/openai-codex"));
    }
}
