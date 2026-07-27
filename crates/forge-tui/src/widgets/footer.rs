//! Footer bar — keyboard hints (mode-aware).

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::path::Path;

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
        if area.height == 1 {
            return;
        }
        let workspace = relative_workspace_label(self.model.cwd.as_str());
        let meta = Line::from(vec![
            Span::styled(self.model.session_short.as_str(), theme::dim()),
            Span::styled(" · ", theme::muted()),
            Span::styled(self.model.status.as_str(), theme::text()),
            Span::styled(" · ", theme::muted()),
            Span::styled(workspace, theme::text()),
            Span::styled(" · ", theme::muted()),
            Span::styled(self.model.hints.as_str(), theme::muted()),
        ]);
        buf.set_line(area.x, area.y, &meta, area.width);
    }
}

fn relative_workspace_label(path: &str) -> String {
    let path = Path::new(path);
    if path.as_os_str().is_empty() {
        return ".".into();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = Path::new(&home);
        if let Ok(rel) = path.strip_prefix(home) {
            let rendered = rel.display().to_string();
            return match rendered.as_str() {
                "" => "~".into(),
                _ => format!("~/{rendered}"),
            };
        }
    }
    if let Ok(current) = std::env::current_dir() {
        if let Ok(rel) = path.strip_prefix(&current) {
            let rendered = rel.display().to_string();
            return if rendered.is_empty() {
                ".".into()
            } else {
                rendered
            };
        }
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| ".".into())
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
            cwd: "/tmp/workspace".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 79, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.trim().is_empty());
    }

    #[test]
    fn renders_workspace_metadata_inline_without_label() {
        let model = FooterModel {
            cwd: "/tmp/workspace".into(),
            hints: "test".into(),
            session_short: "abcd".into(),
            status: "idle".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 80, 2);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("abcd"));
        assert!(rendered.contains("idle"));
        assert!(rendered.contains("workspace"));
        assert!(!rendered.contains("Workspace"));
    }
}
