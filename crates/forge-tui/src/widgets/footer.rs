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
        let limits = compact_limits(usage, weekly_limit, credits);
        let details = [usage_summary, limits.as_str()]
            .into_iter()
            .filter(|detail| !detail.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
        if details.is_empty() {
            buf.set_line(area.x, area.y, &meta, area.width);
            return;
        }

        render_meta_with_details(area, buf, &meta, self.model, details.as_str());
    }
}

fn render_meta_with_details(
    area: Rect,
    buf: &mut Buffer,
    meta: &Line<'_>,
    model: &FooterModel,
    details: &str,
) {
    let details_width = details.chars().count() as u16;
    let model_name = model
        .model
        .rsplit('/')
        .next()
        .unwrap_or(model.model.as_str());
    let compact_meta = Line::from(vec![
        Span::styled(model.status.as_str(), theme::text()),
        Span::styled(" · ", theme::muted()),
        Span::styled(model_name, theme::metadata_style()),
        Span::styled(" · ", theme::muted()),
        Span::styled(format!("{:.0}% ctx", model.ctx_pct * 100.0), theme::muted()),
    ]);
    if details_width.saturating_add(2) >= area.width {
        let meta_width = (area.width / 3).max(12).min(compact_meta.width() as u16);
        let details_area = area.width.saturating_sub(meta_width + 2);
        buf.set_line(area.x, area.y, &compact_meta, meta_width);
        buf.set_stringn(
            area.x + meta_width + 2,
            area.y,
            details,
            details_area as usize,
            theme::muted(),
        );
        return;
    }

    let max_left = area.width.saturating_sub(details_width + 2);
    if meta.width() as u16 <= max_left {
        buf.set_line(area.x, area.y, meta, max_left);
    } else {
        buf.set_line(area.x, area.y, &compact_meta, max_left);
    }
    buf.set_stringn(
        area.x + area.width - details_width,
        area.y,
        details,
        details_width as usize,
        theme::muted(),
    );
}

fn compact_limits(session: &str, weekly: &str, credits: &str) -> String {
    [
        compact_limit("session", session),
        compact_limit("weekly", weekly),
        compact_credits(credits),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn compact_limit(label: &str, value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let remaining = value
        .split_once(':')
        .map(|(_, detail)| detail.trim())
        .unwrap_or(value)
        .split_once("% remaining")
        .map(|(percent, _)| percent.trim());
    let reset = value
        .split_once("resets in ")
        .map(|(_, duration)| duration.trim());

    Some(match (remaining, reset) {
        (Some(percent), Some("unknown")) => format!("{label} {percent}%"),
        (Some(percent), Some(duration)) => format!("{label} {percent}% ({duration})"),
        _ => value.to_owned(),
    })
}

fn compact_credits(value: &str) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let balance = value
        .split_once(':')
        .map(|(_, detail)| detail.trim())
        .unwrap_or(value);
    Some(format!("cr {balance}"))
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
    fn renders_token_usage_and_provider_limits() {
        let model = FooterModel {
            status: "idle".into(),
            provider: "native".into(),
            model: "openai-codex/gpt-5.6-sol".into(),
            effort: "high".into(),
            hints: String::new(),
            cwd: "/tmp/workspace".into(),
            usage_summary: "in 7 · out 5 · total 12".into(),
            usage: "Session limit: 75% remaining (25% used) · resets in 2h".into(),
            weekly_limit: "Weekly limit: 59.5% remaining (40.5% used) · resets in 5d".into(),
            credits: "Credit balance: 12.5".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 100, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("in 7 · out 5 · total 12"));
        assert!(rendered.contains("session 75% (2h)"));
        assert!(rendered.contains("weekly 59.5% (5d)"));
        assert!(rendered.contains("cr 12.5"));
    }

    #[test]
    fn renders_usage_summary_when_provider_limits_are_unavailable() {
        let model = FooterModel {
            status: "idle".into(),
            usage_summary: "in 7 · out 5 · total 12".into(),
            ..FooterModel::default()
        };
        let area = Rect::new(0, 0, 60, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("in 7 · out 5 · total 12"));
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
