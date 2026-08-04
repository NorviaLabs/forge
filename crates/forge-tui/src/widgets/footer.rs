//! Contextual hint slot, and — when no hint is active — the persistent
//! `[vendor/route] [model] [effort]` steady-state control.

use crate::theme;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::Widget;

#[derive(Debug, Clone)]
pub struct FooterModel {
    pub cwd: String,
    pub session_short: String,
    pub status: String,
    pub status_busy: bool,
    /// Active vendor label (e.g. "Anthropic"), or empty when unknown.
    pub provider: String,
    /// Route label when the vendor has more than one offering (e.g. "API
    /// key" vs "ChatGPT sign-in"). `None` for single-route vendors — the
    /// vendor label alone disambiguates.
    pub route_label: Option<String>,
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
            route_label: None,
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

/// One bracketed `[vendor/route]`/`[model]`/`[effort]` segment of the
/// persistent control, and the display column range it occupies — the same
/// rects are used both to paint the segment and to register its mouse hit
/// region, so the two can never drift apart.
pub struct FooterControlSegment {
    pub text: String,
    /// Display-column start/end (end-exclusive), relative to the footer
    /// row's own area.
    pub start: u16,
    pub end: u16,
}

/// Build the `[vendor/route] [model] [effort]` segments that fit in
/// `width`, dropping trailing segments (effort first, then model, then
/// vendor/route) when they don't. Pure layout logic shared by the widget's
/// `render` and the app's mouse-hit-region registration.
pub fn footer_control_segments(model: &FooterModel, width: u16) -> Vec<FooterControlSegment> {
    if !model.connected || model.provider.is_empty() || model.model.is_empty() {
        return Vec::new();
    }
    let vendor_route = match &model.route_label {
        Some(route) if !route.is_empty() => format!("[{} · {route}]", model.provider),
        _ => format!("[{}]", model.provider),
    };
    let mut candidates = vec![vendor_route, format!("[{}]", model.model)];
    if !model.effort.is_empty() {
        candidates.push(format!("[{}]", model.effort));
    }
    if model.ctx_total > 0 {
        let pct = (model.ctx_pct * 100.0).clamp(0.0, 100.0);
        candidates.push(format!("[{pct:.0}%]"));
    }

    // Drop trailing segments (context, effort, then model) until what's left fits.
    while candidates.len() > 1 {
        let joined_width: usize = candidates.iter().map(|s| s.chars().count() + 1).sum();
        if joined_width.saturating_sub(1) <= width as usize {
            break;
        }
        candidates.pop();
    }

    let mut segments = Vec::with_capacity(candidates.len());
    let mut col = 0u16;
    for text in candidates {
        let len = text.chars().count() as u16;
        if col + len > width {
            break;
        }
        let end = col + len;
        segments.push(FooterControlSegment {
            text,
            start: col,
            end,
        });
        col = end + 1; // one-space gap between segments
    }
    segments
}

impl Widget for FooterBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        theme::fill(area, buf, theme::canvas());
        if !self.model.hints.is_empty() {
            buf.set_stringn(
                area.x,
                area.y,
                self.model.hints.as_str(),
                area.width as usize,
                theme::muted(),
            );
            return;
        }
        for segment in footer_control_segments(self.model, area.width) {
            buf.set_span(
                area.x + segment.start,
                area.y,
                &Span::styled(segment.text, theme::text()),
                area.width.saturating_sub(segment.start),
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
            route_label: None,
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

    fn connected_model() -> FooterModel {
        FooterModel {
            connected: true,
            provider: "Anthropic".into(),
            route_label: None,
            model: "claude-sonnet-4-6".into(),
            effort: "Low".into(),
            ..FooterModel::default()
        }
    }

    #[test]
    fn renders_compact_control_when_connected_and_no_hint() {
        let model = connected_model();
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Anthropic"), "{rendered}");
        assert!(rendered.contains("claude-sonnet-4-6"), "{rendered}");
        assert!(rendered.contains("Low"), "{rendered}");
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
            ..connected_model()
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);

        FooterBar { model: &model }.render(area, &mut buf);

        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Enter confirm"));
        assert!(!rendered.contains("claude-sonnet-4-6"));
    }

    #[test]
    fn truncates_effort_segment_first_on_narrow_width() {
        let model = connected_model();

        let wide = footer_control_segments(&model, 80);
        assert_eq!(
            wide.len(),
            3,
            "expected all three segments to fit at width 80"
        );

        let narrow = footer_control_segments(&model, 32);
        assert_eq!(
            narrow.len(),
            2,
            "expected effort to drop first at width 32: {:?}",
            narrow.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert!(narrow.iter().any(|s| s.text.contains("claude-sonnet-4-6")));
        assert!(!narrow.iter().any(|s| s.text.contains("Low")));

        let with_context = FooterModel {
            ctx_total: 8192,
            ctx_pct: 0.42,
            ..connected_model()
        };
        let segments = footer_control_segments(&with_context, 80);
        assert!(
            segments.iter().any(|s| s.text.contains("42%")),
            "expected context chip: {:?}",
            segments.iter().map(|s| &s.text).collect::<Vec<_>>()
        );

        let narrower = footer_control_segments(&model, 15);
        assert_eq!(
            narrower.len(),
            1,
            "expected model to drop next at width 15: {:?}",
            narrower.iter().map(|s| &s.text).collect::<Vec<_>>()
        );
        assert!(narrower[0].text.contains("Anthropic"));
    }

    #[test]
    fn vendor_route_segment_includes_route_label_when_present() {
        let model = FooterModel {
            route_label: Some("ChatGPT sign-in".into()),
            ..connected_model()
        };
        let segments = footer_control_segments(&model, 80);
        assert!(segments[0].text.contains("Anthropic · ChatGPT sign-in"));
    }
}
