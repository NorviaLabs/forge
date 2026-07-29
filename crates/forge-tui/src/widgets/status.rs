//! Session chrome data for `/status`.

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
#[allow(dead_code)]
pub struct StatusModel {
    pub status: SessionStatus,
    pub session_short: String,
    pub model: String,
    pub provider: String,
    pub effort: String,
    pub ctx_pct: f64,
    pub busy: bool,
    pub busy_phase: BusyPhase,
    pub connect_profile: Option<String>,
    /// Whether an LLM provider is usable for chat (connect profile live, or mock).
    pub provider_connected: bool,
    pub web_search_label: Option<String>,
    pub tools_visible: usize,
    pub prompt_cache_hits: u64,
    pub prompt_cache_writes: u64,
    pub repo_name: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
}

impl StatusModel {
    pub fn status_label(&self) -> (String, ratatui::style::Style) {
        self.status_label_with_busy_detail(None)
    }

    pub fn current_state_label(&self) -> &'static str {
        match self.status {
            SessionStatus::AwaitingHitl => "Waiting for you",
            SessionStatus::Failed => "Failed",
            SessionStatus::Completed => "Completed",
            SessionStatus::Running => {
                if self.busy {
                    match &self.busy_phase {
                        BusyPhase::Idle => "Idle",
                        BusyPhase::Model => "Implementing",
                        BusyPhase::Connect => "Waiting for you",
                        BusyPhase::Tool { name } => match name.as_str() {
                            "read_file" | "fffind" | "ffgrep" | "web_search" => "Exploring",
                            "git" => "Validating",
                            _ => "Implementing",
                        },
                        BusyPhase::Other(step) => match step.as_str() {
                            "git sync" => "Validating",
                            _ => "Implementing",
                        },
                    }
                } else {
                    "Idle"
                }
            }
        }
    }

    pub fn repo_branch_label(&self) -> Option<String> {
        let repo = self
            .repo_name
            .as_deref()
            .filter(|value| !value.is_empty())?;
        let branch = self.branch.as_deref().filter(|value| !value.is_empty());
        let mut text = match branch {
            Some(branch) => format!("{repo}/{branch}"),
            None => repo.to_string(),
        };
        if self.dirty {
            text.push('*');
        }
        Some(text)
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
            SessionStatus::Running => ("Idle".into(), theme::ok()),
            SessionStatus::Completed => ("Completed".into(), theme::ok()),
            SessionStatus::Failed => ("Failed".into(), theme::danger()),
            SessionStatus::AwaitingHitl => (
                "Waiting for you".into(),
                theme::warn().add_modifier(Modifier::BOLD),
            ),
        }
    }

    #[allow(dead_code)]
    fn truncate_model(model: &str, max: usize) -> String {
        #[allow(dead_code)]
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

    fn truncate_middle(text: &str, max: usize) -> String {
        let n = text.chars().count();
        if n <= max {
            return text.to_string();
        }
        if max < 5 {
            return text.chars().take(max).collect();
        }
        let keep = (max - 1) / 2;
        let start: String = text.chars().take(keep).collect();
        let end: String = text
            .chars()
            .rev()
            .take(max - keep - 1)
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
        let mut spans = vec![Span::styled(
            "Forge",
            theme::brand().add_modifier(Modifier::BOLD),
        )];
        let repo = self.model.repo_branch_label();
        let model = self
            .model
            .model
            .rsplit('/')
            .next()
            .unwrap_or(self.model.model.as_str())
            .to_string();
        let ctx = format!("{:.0}% context", self.model.ctx_pct * 100.0);
        let state = self.model.current_state_label().to_string();

        let separators = "  ";
        let mut used = spans[0].content.chars().count();
        let state_field = (state, theme::info().add_modifier(Modifier::BOLD));
        let state_needed = separators.chars().count() + state_field.0.chars().count();

        if let Some(repo) = repo {
            let reserve = state_needed + separators.chars().count() + 6;
            let available_repo = (area.width as usize)
                .saturating_sub(used + separators.chars().count())
                .saturating_sub(reserve)
                .max(8);
            let repo = StatusModel::truncate_middle(&repo, available_repo);
            let needed = separators.chars().count() + repo.chars().count();
            if used + needed <= area.width as usize {
                spans.push(Span::raw(separators));
                spans.push(Span::styled(repo, theme::text()));
                used += needed;
            }
        }

        if used + state_needed <= area.width as usize {
            let model_needed = separators.chars().count() + model.chars().count();
            let ctx_needed = separators.chars().count() + ctx.chars().count();
            if used + model_needed + ctx_needed + state_needed <= area.width as usize {
                spans.push(Span::raw(separators));
                spans.push(Span::styled(model, theme::metadata_style()));
                spans.push(Span::raw(separators));
                spans.push(Span::styled(ctx, theme::muted()));
            } else if used + model_needed + state_needed <= area.width as usize {
                spans.push(Span::raw(separators));
                spans.push(Span::styled(model, theme::metadata_style()));
            }
            spans.push(Span::raw(separators));
            spans.push(Span::styled(state_field.0, state_field.1));
        }

        buf.set_line(area.x, area.y, &Line::from(spans), area.width);
    }
}

/// Build chrome from app-facing fields (single source for status + /status).
pub fn session_chrome_lines(m: &StatusModel) -> Vec<String> {
    let (label, _) = m.status_label();
    vec![
        format!("status={label}"),
        format!("provider={}", m.provider),
        format!("model={}", m.model),
        format!("effort={}", m.effort),
        format!("ctx={:.1}%", m.ctx_pct * 100.0),
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
    use ratatui::buffer::Buffer;

    fn status_model(status: SessionStatus, busy: bool, busy_phase: BusyPhase) -> StatusModel {
        StatusModel {
            status,
            session_short: "abc".into(),
            model: "openai/gpt-5".into(),
            provider: "native".into(),
            effort: "auto".into(),
            ctx_pct: 0.32,
            busy,
            busy_phase,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
        }
    }

    #[test]
    fn hitl_label() {
        let m = StatusModel {
            status: SessionStatus::AwaitingHitl,
            session_short: "abcd".into(),
            model: "mock".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.1,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
        };
        assert_eq!(m.status_label().0, "Waiting for you");
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
            busy: true,
            busy_phase: BusyPhase::Model,
            connect_profile: None,
            provider_connected: false,
            web_search_label: Some("mock".into()),
            tools_visible: 5,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
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
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: Some("xai".into()),
            provider_connected: true,
            web_search_label: Some("mock".into()),
            tools_visible: 4,
            prompt_cache_hits: 2,
            prompt_cache_writes: 1,
            repo_name: None,
            branch: None,
            dirty: false,
        };
        let lines = session_chrome_lines(&m);
        assert!(lines.iter().any(|l| l.contains("provider=native")));
        assert!(lines.iter().any(|l| l.contains("model=openai/gpt")));
        assert!(lines.iter().any(|l| l.contains("effort=medium")));
        assert!(lines.iter().any(|l| l.contains("profile=xai")));
        assert!(lines.iter().any(|l| l.contains("connected=yes")));
    }

    #[test]
    fn status_bar_uses_connect_profile_and_model_name() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "abc".into(),
            model: "openai-code/gpt-5.4".into(),
            provider: "native".into(),
            effort: "medium".into(),
            ctx_pct: 0.34,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: Some("openai-code".into()),
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: true,
        };

        assert_eq!(m.connect_profile.as_deref(), Some("openai-code"));
        assert_eq!(m.model.rsplit('/').next(), Some("gpt-5.4"));
        assert_eq!(m.repo_branch_label().as_deref(), Some("forge/main*"));
    }

    #[test]
    fn truncate_long_model() {
        let s = StatusModel::truncate_model("openai/very-long-model-name-here", 12);
        assert!(s.contains('…'));
        assert!(s.chars().count() <= 12);
    }

    #[test]
    fn truncate_middle_keeps_edges() {
        let s = StatusModel::truncate_middle("forge/very-long-branch-name", 12);
        assert!(s.contains('…'));
        assert!(s.chars().count() <= 12);
    }

    #[test]
    fn awaiting_hitl_maps_to_waiting_for_you() {
        let m = StatusModel {
            status: SessionStatus::AwaitingHitl,
            session_short: "abc".into(),
            model: "mock".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.32,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: false,
        };
        assert_eq!(m.current_state_label(), "Waiting for you");
    }

    #[test]
    fn status_bar_renders_full_header_when_wide() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "abc".into(),
            model: "mock".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.32,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: true,
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Forge"));
        assert!(rendered.contains("forge/main*"));
        assert!(rendered.contains("mock"));
        assert!(rendered.contains("32% context"));
        assert!(rendered.contains("Idle"));
    }

    #[test]
    fn status_bar_preserves_state_on_narrow_width() {
        let m = StatusModel {
            status: SessionStatus::Running,
            session_short: "abc".into(),
            model: "very-long-model-name".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.32,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: false,
        };
        let area = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Forge"));
        assert!(rendered.contains("Idle"));
    }

    #[test]
    fn current_state_label_covers_status_and_busy_phase_branches() {
        assert_eq!(
            status_model(SessionStatus::Completed, false, BusyPhase::Idle).current_state_label(),
            "Completed"
        );
        assert_eq!(
            status_model(SessionStatus::Failed, false, BusyPhase::Idle).current_state_label(),
            "Failed"
        );
        assert_eq!(
            status_model(SessionStatus::Running, true, BusyPhase::Idle).current_state_label(),
            "Idle"
        );
        assert_eq!(
            status_model(SessionStatus::Running, true, BusyPhase::Connect).current_state_label(),
            "Waiting for you"
        );
        assert_eq!(
            status_model(
                SessionStatus::Running,
                true,
                BusyPhase::Tool {
                    name: "read_file".into()
                }
            )
            .current_state_label(),
            "Exploring"
        );
        assert_eq!(
            status_model(
                SessionStatus::Running,
                true,
                BusyPhase::Tool { name: "git".into() }
            )
            .current_state_label(),
            "Validating"
        );
        assert_eq!(
            status_model(
                SessionStatus::Running,
                true,
                BusyPhase::Tool {
                    name: "custom".into()
                }
            )
            .current_state_label(),
            "Implementing"
        );
        assert_eq!(
            status_model(
                SessionStatus::Running,
                true,
                BusyPhase::Other("git sync".into())
            )
            .current_state_label(),
            "Validating"
        );
        assert_eq!(BusyPhase::Tool { name: "git".into() }.label(), "tool:git");
        assert_eq!(BusyPhase::Connect.label(), "connect");
        assert_eq!(BusyPhase::Other("phase".into()).label(), "phase");
    }

    #[test]
    fn status_label_and_truncation_cover_edge_cases() {
        let busy = status_model(
            SessionStatus::Running,
            true,
            BusyPhase::Other(String::new()),
        );
        assert!(busy
            .status_label_with_busy_detail(None)
            .0
            .contains("running"));
        assert!(busy
            .status_label_with_busy_detail(Some("custom detail"))
            .0
            .contains("custom detail"));

        assert_eq!(
            status_model(SessionStatus::Running, false, BusyPhase::Idle)
                .status_label()
                .0,
            "Idle"
        );
        assert_eq!(
            status_model(SessionStatus::Completed, false, BusyPhase::Idle)
                .status_label()
                .0,
            "Completed"
        );
        assert_eq!(
            status_model(SessionStatus::Failed, false, BusyPhase::Idle)
                .status_label()
                .0,
            "Failed"
        );

        assert_eq!(StatusModel::truncate_model("abcdef", 3), "abc");
        assert_eq!(StatusModel::truncate_middle("abcdef", 4), "abcd");
        assert_eq!(StatusModel::truncate_middle("abcdef", 5), "ab…ef");

        let mut m = status_model(SessionStatus::Running, false, BusyPhase::Idle);
        assert_eq!(m.repo_branch_label(), None);
        m.repo_name = Some("forge".into());
        assert_eq!(m.repo_branch_label().as_deref(), Some("forge"));
    }

    #[test]
    fn status_bar_handles_zero_sized_area() {
        let m = status_model(SessionStatus::Running, false, BusyPhase::Idle);
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
    }
}
