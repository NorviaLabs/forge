//! Footer bar — keyboard hints (mode-aware).

use crate::theme;
use pathdiff::diff_paths;
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
            hints: "Enter send · ⇧Enter newline · Ctrl+K cmds · Esc clear".into(),
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

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let workspace = compact_path_label(self.model.cwd.as_str());
        let identity = self.model.identity_summary();
        let meta = Line::from(vec![
            Span::styled(self.model.status.as_str(), theme::text()),
            Span::styled(" · ", theme::muted()),
            Span::styled(identity, theme::metadata_style()),
            Span::styled(" · ", theme::muted()),
            Span::styled(workspace.as_str(), theme::text()),
            Span::styled(" · ", theme::muted()),
            Span::styled(self.model.hints.as_str(), theme::muted()),
        ]);
        let usage = self.model.usage.as_str();
        let weekly_limit = self.model.weekly_limit.as_str();
        let credits = self.model.credits.as_str();
        let usage_summary = self.model.usage_summary.as_str();
        if usage.is_empty() && weekly_limit.is_empty() && credits.is_empty() && usage_summary.is_empty() {
            buf.set_line(area.x, area.y, &meta, area.width);
            return;
        }

        let usage_text = if !usage.is_empty() || !weekly_limit.is_empty() || !credits.is_empty() {
            format!("{usage} · {weekly_limit} · {credits}")
        } else {
            usage_summary.to_owned()
        };
        let usage_width = usage_text.chars().count() as u16;
        if usage_width.saturating_add(2) >= area.width {
            buf.set_line(area.x, area.y, &meta, area.width);
            return;
        }

        let left_width = meta.width() as u16;
        let max_left = area.width.saturating_sub(usage_width + 2);
        if left_width <= max_left {
            buf.set_line(area.x, area.y, &meta, max_left);
            buf.set_stringn(
                area.x + area.width - usage_width,
                area.y,
                usage_text.as_str(),
                usage_width as usize,
                theme::muted(),
            );
        } else {
            let compact_meta = Line::from(vec![
                Span::styled(self.model.status.as_str(), theme::text()),
                Span::styled(" · ", theme::muted()),
                Span::styled(self.model.identity_summary(), theme::metadata_style()),
                Span::styled(" · ", theme::muted()),
                Span::styled(workspace, theme::text()),
            ]);
            buf.set_line(area.x, area.y, &compact_meta, max_left);
            buf.set_stringn(
                area.x + area.width - usage_width,
                area.y,
                usage_text.as_str(),
                usage_width as usize,
                theme::muted(),
            );
        }
    }
}

impl FooterModel {
    fn identity_summary(&self) -> String {
        let provider = if self.provider == "native" {
            self.connect_profile.as_deref().unwrap_or("native")
        } else if self.provider.is_empty() {
            "—"
        } else {
            self.provider.as_str()
        };
        let model = self.model.rsplit('/').next().unwrap_or(self.model.as_str());
        let ctx = format!("{:.0}% ctx", self.ctx_pct * 100.0);
        format!("{provider} · {model} · {} · {ctx}", self.effort)
    }
}

fn compact_path_label(path: &str) -> String {
    let home = dirs::home_dir();
    let cwd = std::env::current_dir().ok();
    compact_path_label_with_anchors(Path::new(path), home.as_deref(), cwd.as_deref())
}

fn compact_path_label_with_anchors(path: &Path, home: Option<&Path>, cwd: Option<&Path>) -> String {
    if path.as_os_str().is_empty() {
        return ".".into();
    }
    if let Some(home) = home {
        if let Some(rel) = diff_paths(path, home) {
            let rendered = rel.display().to_string();
            return match rendered.as_str() {
                "" => "~".into(),
                "." => "~".into(),
                _ => format!("~/{rendered}"),
            };
        }
    }
    if let Some(current) = cwd {
        if let Some(rel) = diff_paths(path, current) {
            let rendered = rel.display().to_string();
            return if rendered.is_empty() || rendered == "." {
                ".".into()
            } else {
                rendered
            };
        }
    }
    path.display().to_string()
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
        assert!(rendered.contains("thinking"));
    }

    #[test]
    fn does_not_repeat_status_metadata() {
        let model = FooterModel {
            status: "idle".into(),
            provider: "native".into(),
            model: "openai-codex/gpt-5.6-sol".into(),
            effort: "high".into(),
            hints: String::new(),
            cwd: "/tmp/workspace".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 79, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("idle"));
        assert!(rendered.contains("native"));
        assert!(rendered.contains("gpt-5.6-sol"));
    }

    #[test]
    fn compact_path_prefers_home_then_current_directory() {
        let home = std::env::temp_dir().join("homeish");
        let current = std::env::temp_dir().join("workspaceish");
        let path = home.join("Projects/forge");
        let rendered =
            compact_path_label_with_anchors(&path, Some(home.as_path()), Some(current.as_path()));
        assert_eq!(rendered, "~/Projects/forge");
    }
}
