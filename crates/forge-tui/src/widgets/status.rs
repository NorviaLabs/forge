//! Status bar — calm single chrome line (polish).

use crate::theme;
use forge_types::SessionStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::time::{SystemTime, UNIX_EPOCH};

/// Progressive busy phase (Phase 10 / TUI-10; also used in chrome label).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BusyPhase {
    #[default]
    Idle,
    Model,
    Tool {
        name: String,
    },
    Connect,
    Other(String),
}

impl BusyPhase {
    pub fn label(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Model => "thinking".into(),
            Self::Tool { name } => format!("tool:{name}"),
            Self::Connect => "connect".into(),
            Self::Other(s) => s.clone(),
        }
    }
}

const SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn spinner_frame() -> &'static str {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[((ms / 80) as usize) % SPINNER.len()]
}

#[derive(Debug, Clone)]
pub struct StatusModel {
    pub status: SessionStatus,
    pub session_short: String,
    pub model: String,
    pub provider: String,
    pub effort: String,
    pub ctx_pct: f64,
    pub worktree_on: bool,
    pub busy: bool,
    pub busy_phase: BusyPhase,
    pub connect_profile: Option<String>,
    /// Whether an LLM provider is usable for chat (connect profile live, or mock).
    pub provider_connected: bool,
    pub web_search_label: Option<String>,
    pub tools_visible: usize,
    pub prompt_cache_hits: u64,
    pub prompt_cache_writes: u64,
}

impl StatusModel {
    pub fn status_label(&self) -> (String, ratatui::style::Style) {
        self.status_label_with_busy_detail(None)
    }

    pub fn status_label_with_busy_detail(
        &self,
        busy_detail: Option<&str>,
    ) -> (String, ratatui::style::Style) {
        if self.busy {
            let phase = self.busy_phase.label();
            let spin = spinner_frame();
            let text = if let Some(detail) = busy_detail.filter(|detail| !detail.is_empty()) {
                format!("{spin} {detail}")
            } else if phase.is_empty() {
                format!("{spin} running")
            } else {
                format!("{spin} {phase}")
            };
            return (text, theme::info().add_modifier(Modifier::BOLD));
        }
        match self.status {
            SessionStatus::Running => ("idle".into(), theme::ok()),
            SessionStatus::Completed => ("done".into(), theme::ok()),
            SessionStatus::Failed => ("failed".into(), theme::danger()),
            SessionStatus::AwaitingHitl => (
                "awaiting".into(),
                theme::warn().add_modifier(Modifier::BOLD),
            ),
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
        let model_disp = StatusModel::truncate_model(&self.model.model, 24);
        let provider = if self.model.provider.is_empty() {
            "—"
        } else {
            self.model.provider.as_str()
        };
        let ctx = format!("ctx {:.1}%", self.model.ctx_pct * 100.0);
        let cache = format!(
            "cache {} / {}",
            self.model.prompt_cache_hits, self.model.prompt_cache_writes
        );

        // Branded, compact chrome: forge · state · model · ctx/cache · connection.
        let (conn_label, conn_style) = if self.model.provider_connected {
            let who = self.model.connect_profile.as_deref().unwrap_or(
                if self.model.provider.eq_ignore_ascii_case("mock") {
                    "mock"
                } else {
                    "ready"
                },
            );
            (format!(" connected:{who} "), theme::ok())
        } else {
            (
                " not connected ".into(),
                theme::warn().add_modifier(Modifier::BOLD),
            )
        };

        let mut spans = vec![
            Span::styled(" forge ", theme::brand()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("[{label}] "), style),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("{provider}/{model_disp} "), theme::text()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("[{ctx}] "), theme::info()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("[{cache}] "), theme::muted()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("effort={} ", self.model.effort), theme::text()),
            Span::styled("· ", theme::dim()),
            Span::styled(format!("[{conn_label}] "), conn_style),
        ];

        if self.model.worktree_on {
            spans.push(Span::styled("· ", theme::dim()));
            spans.push(Span::styled("worktree ", theme::warn()));
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
        format!("effort={}", m.effort),
        format!("ctx={:.1}%", m.ctx_pct * 100.0),
        format!("worktree={}", if m.worktree_on { "on" } else { "off" }),
        format!("profile={}", m.connect_profile.as_deref().unwrap_or("—")),
        format!(
            "connected={}",
            if m.provider_connected { "yes" } else { "no" }
        ),
        format!(
            "web_search={}",
            m.web_search_label.as_deref().unwrap_or("off")
        ),
        format!("tools={}", m.tools_visible),
        format!(
            "prompt_cache=hits:{} writes:{}",
            m.prompt_cache_hits, m.prompt_cache_writes
        ),
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
            effort: "auto".into(),
            ctx_pct: 0.1,
            worktree_on: false,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
        };
        assert_eq!(m.status_label().0, "awaiting");
    }

    #[test]
    fn busy_phase_model() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "x".into(),
            model: "m".into(),
            provider: "native".into(),
            effort: "high".into(),
            ctx_pct: 0.0,
            worktree_on: true,
            busy: true,
            busy_phase: BusyPhase::Model,
            connect_profile: None,
            provider_connected: false,
            web_search_label: Some("mock".into()),
            tools_visible: 5,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
        };
        assert!(m.status_label().0.contains("thinking"));
    }

    #[test]
    fn chrome_lines_include_provider_model() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "abc".into(),
            model: "openai/gpt".into(),
            provider: "native".into(),
            effort: "medium".into(),
            ctx_pct: 0.34,
            worktree_on: false,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: Some("xai".into()),
            provider_connected: true,
            web_search_label: Some("mock".into()),
            tools_visible: 4,
            prompt_cache_hits: 2,
            prompt_cache_writes: 1,
        };
        let lines = session_chrome_lines(&m);
        assert!(lines.iter().any(|l| l.contains("provider=native")));
        assert!(lines.iter().any(|l| l.contains("model=openai/gpt")));
        assert!(lines.iter().any(|l| l.contains("effort=medium")));
        assert!(lines.iter().any(|l| l.contains("profile=xai")));
        assert!(lines.iter().any(|l| l.contains("connected=yes")));
    }

    #[test]
    fn truncate_long_model() {
        let s = StatusModel::truncate_model("openai/very-long-model-name-here", 12);
        assert!(s.contains('…'));
        assert!(s.chars().count() <= 12);
    }
}
