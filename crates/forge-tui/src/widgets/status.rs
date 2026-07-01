//! Status bar widget (TUI-01 + Phase 10 / TUI-09 session chrome).

use crate::theme;
use forge_types::SessionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

/// Progressive busy phase (Phase 10 / TUI-10; also used in chrome label).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BusyPhase {
    #[default]
    Idle,
    Model,
    Tool { name: String },
    Connect,
    Other(String),
}

impl BusyPhase {
    pub fn label(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Model => "running · model".into(),
            Self::Tool { name } => format!("running · tool:{name}"),
            Self::Connect => "running · connect".into(),
            Self::Other(s) => format!("running · {s}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusModel {
    pub status: SessionStatus,
    pub session_short: String,
    pub model: String,
    /// Phase 10 — provider next to model.
    pub provider: String,
    pub ctx_pct: f64,
    pub worktree_on: bool,
    pub busy: bool,
    /// Phase 10 progressive busy (optional detail).
    pub busy_phase: BusyPhase,
    /// Active connect profile id, if any.
    pub connect_profile: Option<String>,
    /// web_search backend label or "off".
    pub web_search_label: Option<String>,
    pub tools_visible: usize,
}

impl StatusModel {
    pub fn status_label(&self) -> (String, ratatui::style::Style) {
        if self.busy {
            let phase = self.busy_phase.label();
            let text = if phase.is_empty() {
                "running".into()
            } else {
                phase
            };
            return (text, theme::info().add_modifier(Modifier::BOLD));
        }
        match self.status {
            SessionStatus::Running => ("idle".into(), theme::ok()),
            SessionStatus::Completed => ("completed".into(), theme::ok()),
            SessionStatus::Failed => ("failed".into(), theme::danger()),
            SessionStatus::AwaitingHitl => (
                "awaiting_hitl".into(),
                theme::warn().add_modifier(Modifier::BOLD),
            ),
        }
    }

    fn ctx_style(&self) -> ratatui::style::Style {
        let r = self.ctx_pct.clamp(0.0, 1.0);
        if r >= 0.90 {
            theme::danger()
        } else if r >= 0.70 {
            theme::warn()
        } else {
            theme::muted()
        }
    }

    fn truncate_model(model: &str, max: usize) -> String {
        let n = model.chars().count();
        if n <= max {
            return model.to_string();
        }
        if max < 8 {
            return model.chars().take(max).collect();
        }
        let keep = (max - 1) / 2;
        let start: String = model.chars().take(keep).collect();
        let end: String = model
            .chars()
            .rev()
            .take(keep)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("{start}…{end}")
    }
}

pub struct StatusBar<'a> {
    pub model: &'a StatusModel,
}

impl Widget for StatusBar<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (label, style) = self.model.status_label();
        let wt = if self.model.worktree_on {
            "wt on"
        } else {
            "wt off"
        };
        let provider = if self.model.provider.is_empty() {
            "—"
        } else {
            self.model.provider.as_str()
        };
        let model_disp = StatusModel::truncate_model(&self.model.model, 28);
        let ctx = format!("ctx {:.0}%", self.model.ctx_pct * 100.0);

        // Priority packing: brand · pill · provider · model · ctx always first.
        let mut spans = vec![
            Span::styled(" FORGE ", theme::brand()),
            Span::styled("│ ", theme::dim()),
            Span::styled(format!(" {label} "), style),
            Span::styled(" │ ", theme::dim()),
            Span::styled(
                format!("sess {}", self.model.session_short),
                theme::info(),
            ),
            Span::styled(" │ ", theme::dim()),
            Span::styled(format!("{provider} · {model_disp}"), theme::text()),
            Span::styled(" │ ", theme::dim()),
            Span::styled(ctx, self.model.ctx_style()),
            Span::styled(format!(" │ {wt} "), theme::muted()),
        ];

        // Extra tokens when width allows (approx by char budget).
        let w = area.width as usize;
        if w >= 100 {
            if let Some(ref p) = self.model.connect_profile {
                spans.push(Span::styled(
                    format!("│ profile {p} "),
                    theme::muted(),
                ));
            }
            if let Some(ref s) = self.model.web_search_label {
                spans.push(Span::styled(format!("│ search {s} "), theme::muted()));
            }
            if self.model.tools_visible > 0 {
                spans.push(Span::styled(
                    format!("│ tools {} ", self.model.tools_visible),
                    theme::muted(),
                ));
            }
        }

        let line = Line::from(spans);
        buf.set_line(area.x, area.y, &line, area.width);
    }
}

/// Build chrome from app-facing fields (single source for status + /status).
pub fn session_chrome_lines(m: &StatusModel) -> Vec<String> {
    let (label, _) = m.status_label();
    vec![
        format!("status={label}"),
        format!("session={}", m.session_short),
        format!("provider={}", m.provider),
        format!("model={}", m.model),
        format!("ctx={:.1}%", m.ctx_pct * 100.0),
        format!(
            "worktree={}",
            if m.worktree_on { "on" } else { "off" }
        ),
        format!(
            "profile={}",
            m.connect_profile.as_deref().unwrap_or("—")
        ),
        format!(
            "web_search={}",
            m.web_search_label.as_deref().unwrap_or("off")
        ),
        format!("tools={}", m.tools_visible),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hitl_label() {
        let m = StatusModel {
            status: SessionStatus::AwaitingHitl,
            session_short: "abcd".into(),
            model: "mock".into(),
            provider: "mock".into(),
            ctx_pct: 0.1,
            worktree_on: false,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            web_search_label: None,
            tools_visible: 0,
        };
        assert_eq!(m.status_label().0, "awaiting_hitl");
    }

    #[test]
    fn busy_phase_model() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "x".into(),
            model: "m".into(),
            provider: "litellm".into(),
            ctx_pct: 0.0,
            worktree_on: true,
            busy: true,
            busy_phase: BusyPhase::Model,
            connect_profile: None,
            web_search_label: Some("mock".into()),
            tools_visible: 5,
        };
        assert!(m.status_label().0.contains("model"));
    }

    #[test]
    fn chrome_lines_include_provider_model() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "abc".into(),
            model: "openai/gpt".into(),
            provider: "litellm".into(),
            ctx_pct: 0.34,
            worktree_on: false,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: Some("xai".into()),
            web_search_label: Some("mock".into()),
            tools_visible: 4,
        };
        let lines = session_chrome_lines(&m);
        assert!(lines.iter().any(|l| l.contains("provider=litellm")));
        assert!(lines.iter().any(|l| l.contains("model=openai/gpt")));
        assert!(lines.iter().any(|l| l.contains("profile=xai")));
    }

    #[test]
    fn truncate_long_model() {
        let s = StatusModel::truncate_model("openai/very-long-model-name-here", 12);
        assert!(s.contains('…'));
        assert!(s.chars().count() <= 12);
    }
}
