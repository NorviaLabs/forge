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
/// This is activity detail, not overall turn lifecycle.
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
    /// Internal activity label (activity feed / diagnostics).
    pub fn label(&self) -> String {
        match self {
            Self::Idle => String::new(),
            Self::Model => "thinking".into(),
            Self::Tool { name } => format!("tool:{name}"),
            Self::Connect => "connect".into(),
            Self::Other(s) => s.clone(),
        }
    }

    /// Typed header progress description. Empty when none is safe to show.
    pub fn progress_description(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Model => None,
            Self::Connect => None,
            Self::Other(s) => {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            Self::Tool { name } => Some(tool_progress_description(name)),
        }
    }
}

fn tool_progress_description(name: &str) -> String {
    match name {
        "read_file" => "Reading files".into(),
        "write_file" | "apply_patch" => "Editing files".into(),
        "fffind" | "fffind_files" => "Searching files".into(),
        "ffgrep" | "ffgrep_files" => "Searching code".into(),
        "bash" => "Running command".into(),
        "git" => "Checking git".into(),
        "web_search" => "Searching the web".into(),
        other => {
            let cleaned = other.replace('_', " ");
            if cleaned.is_empty() {
                "Running tool".into()
            } else {
                let mut chars = cleaned.chars();
                match chars.next() {
                    Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
                    None => "Running tool".into(),
                }
            }
        }
    }
}

/// First-class turn lifecycle — separate from tool/activity status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnLifecycle {
    /// No active turn; ready for input.
    Ready,
    /// Agent turn in progress.
    Working,
    /// Blocked on human approval or connect.
    Waiting,
    /// Turn finished successfully with a final answer.
    Completed,
    /// Turn finished in failure (incl. retry exhaustion).
    Failed,
    /// Operator cancelled the in-flight turn.
    Cancelled,
    /// Persisted active task with no recoverable runtime.
    Interrupted,
}

impl TurnLifecycle {
    /// Map authoritative session status + busy/cancel flags.
    /// Does not inspect activity rows or assistant text.
    pub fn from_session(status: SessionStatus, busy: bool, cancelled: bool) -> Self {
        // Explicit durable cancel wins even if a stale busy flag lingers.
        if cancelled || status == SessionStatus::Cancelled {
            return Self::Cancelled;
        }
        match status {
            SessionStatus::AwaitingHitl => Self::Waiting,
            SessionStatus::Failed => Self::Failed,
            SessionStatus::Completed => Self::Completed,
            SessionStatus::Interrupted => Self::Interrupted,
            SessionStatus::Cancelled => Self::Cancelled,
            SessionStatus::Running => {
                if busy {
                    Self::Working
                } else {
                    // Fresh / legacy Running with no live runtime is Ready, not Working.
                    Self::Ready
                }
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::Working => "Working",
            Self::Waiting => "Waiting",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
            Self::Interrupted => "Interrupted",
        }
    }

    pub fn style(self) -> ratatui::style::Style {
        match self {
            Self::Ready => theme::muted(),
            Self::Working => theme::info().add_modifier(Modifier::BOLD),
            Self::Waiting => theme::warn().add_modifier(Modifier::BOLD),
            Self::Completed => theme::ok(),
            Self::Failed => theme::danger(),
            Self::Cancelled => theme::muted(),
            Self::Interrupted => theme::warn(),
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Ready => "",
            Self::Working => "◌",
            Self::Waiting => "!",
            Self::Completed => "✓",
            Self::Failed => "✗",
            Self::Cancelled => "■",
            Self::Interrupted => "!",
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

fn format_lifecycle_label(life: TurnLifecycle, detail: Option<&str>, animated: bool) -> String {
    let glyph = match life {
        TurnLifecycle::Working if animated => spinner_frame(),
        other => other.symbol(),
    };
    let state = life.label();
    let core = if glyph.is_empty() {
        state.to_string()
    } else {
        format!("{glyph} {state}")
    };
    match detail.map(str::trim).filter(|d| !d.is_empty()) {
        Some(detail)
            if matches!(
                life,
                TurnLifecycle::Working | TurnLifecycle::Waiting | TurnLifecycle::Failed
            ) =>
        {
            format!("{core} · {detail}")
        }
        _ => core,
    }
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
    pub resource: Option<String>,
    pub activity: Option<String>,
    /// Soft-cancel of the in-flight turn (Esc while busy) reached a terminal cancel.
    pub turn_cancelled: bool,
    /// Typed progress description for Working (from structured busy phase / progress).
    pub progress_description: Option<String>,
    /// Safe concise failure category for Failed header detail (never raw errors).
    pub failure_category: Option<String>,
    /// Waiting reason detail when blocked on the operator.
    pub waiting_detail: Option<String>,
}

impl StatusModel {
    pub fn status_label(&self) -> (String, ratatui::style::Style) {
        self.status_label_for_width(usize::MAX)
    }

    /// Overall turn lifecycle label (not tool/activity phase).
    pub fn turn_lifecycle(&self) -> TurnLifecycle {
        // Connect-busy is waiting on the operator, not "Working".
        if self.busy && matches!(self.busy_phase, BusyPhase::Connect) {
            return TurnLifecycle::Waiting;
        }
        if self.waiting_detail.is_some()
            && !self.busy
            && matches!(
                self.status,
                SessionStatus::Running | SessionStatus::AwaitingHitl
            )
        {
            return TurnLifecycle::Waiting;
        }
        if self.status == SessionStatus::AwaitingHitl {
            return TurnLifecycle::Waiting;
        }
        TurnLifecycle::from_session(self.status, self.busy, self.turn_cancelled)
    }

    pub fn current_state_label(&self) -> &'static str {
        self.turn_lifecycle().label()
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

    /// Detail text after the lifecycle label (progress / waiting / failure category).
    pub fn status_detail(&self) -> Option<String> {
        match self.turn_lifecycle() {
            TurnLifecycle::Working => self
                .progress_description
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| self.busy_phase.progress_description()),
            TurnLifecycle::Waiting => self
                .waiting_detail
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    if matches!(self.busy_phase, BusyPhase::Connect) {
                        Some("Your input required".into())
                    } else if self.status == SessionStatus::AwaitingHitl {
                        Some("Approval required".into())
                    } else {
                        None
                    }
                }),
            TurnLifecycle::Failed => self
                .failure_category
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string()),
            _ => None,
        }
    }

    pub fn status_label_with_busy_detail(
        &self,
        busy_detail: Option<&str>,
    ) -> (String, ratatui::style::Style) {
        let life = self.turn_lifecycle();
        let detail = busy_detail
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .map(|d| d.to_string())
            .or_else(|| self.status_detail());
        (
            format_lifecycle_label(life, detail.as_deref(), true),
            life.style(),
        )
    }

    /// Width-aware lifecycle label. State label is never truncated.
    pub fn status_label_for_width(&self, max_chars: usize) -> (String, ratatui::style::Style) {
        let life = self.turn_lifecycle();
        let detail = self.status_detail();
        let with_detail = format_lifecycle_label(life, detail.as_deref(), true);
        if with_detail.chars().count() <= max_chars {
            return (with_detail, life.style());
        }
        let base = format_lifecycle_label(life, None, true);
        if base.chars().count() <= max_chars {
            return (base, life.style());
        }
        // Last resort: drop glyph, keep bare state text.
        let bare = life.label().to_string();
        (bare, life.style())
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
        // Priority: state label > product/workspace identity > progress description > secondary meta.
        let width = area.width as usize;
        let separators = "  ";
        let sep_len = separators.chars().count();
        let brand = "Forge";
        let brand_len = brand.chars().count();

        let life = self.model.turn_lifecycle();
        let detail = self.model.status_detail();
        let state_only = format_lifecycle_label(life, None, true);
        let state_with_detail = format_lifecycle_label(life, detail.as_deref(), true);
        let state_bare = life.label().to_string();

        // Always reserve the state label; drop detail first under pressure.
        let life_label = if brand_len + sep_len + state_with_detail.chars().count() <= width {
            state_with_detail
        } else if brand_len + sep_len + state_only.chars().count() <= width {
            state_only
        } else {
            state_bare
        };
        let life_style = life.style();

        let mut spans = vec![Span::styled(
            brand,
            theme::brand().add_modifier(Modifier::BOLD),
        )];
        let mut used = brand_len;

        // Ensure lifecycle fits even if we must drop brand-adjacent metadata.
        let life_needed = sep_len + life_label.chars().count();
        let room_for_repo = width.saturating_sub(used + life_needed);

        if let Some(repo) = self.model.repo_branch_label() {
            if room_for_repo > sep_len {
                let available_repo = room_for_repo.saturating_sub(sep_len).max(0);
                if available_repo >= 4 {
                    let repo = StatusModel::truncate_middle(&repo, available_repo);
                    let needed = sep_len + repo.chars().count();
                    if used + needed + life_needed <= width {
                        spans.push(Span::raw(separators));
                        spans.push(Span::styled(repo, theme::text()));
                        used += needed;
                    }
                }
            }
        }

        // Recompute room after repo.
        if used + life_needed <= width {
            spans.push(Span::raw(separators));
            spans.push(Span::styled(life_label, life_style));
            used += life_needed;
        } else if life_label.chars().count() <= width {
            // Extremely narrow: prefer state over brand if somehow constrained.
            spans.clear();
            used = life_label.chars().count();
            spans.push(Span::styled(life_label, life_style));
        } else {
            spans.push(Span::raw(separators));
            spans.push(Span::styled(life.label().to_string(), life_style));
            used = width;
        }

        // Optional resource (file/run view) only if leftover room remains.
        if let Some(resource) = self
            .model
            .resource
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let available = width.saturating_sub(used + sep_len);
            if available >= 4 {
                let resource = StatusModel::truncate_middle(resource, available);
                let needed = sep_len + resource.chars().count();
                if used + needed <= width {
                    spans.push(Span::raw(separators));
                    spans.push(Span::styled(resource, theme::metadata_style()));
                    used += needed;
                }
            }
        }

        // Workspace activity is secondary metadata — never displaces lifecycle.
        if let Some(activity) = self
            .model
            .activity
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            let needed = sep_len + activity.chars().count();
            if used + needed <= width {
                spans.push(Span::raw(separators));
                spans.push(Span::styled(activity.to_string(), theme::metadata_style()));
            }
        }

        theme::fill(area, buf, theme::canvas());
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
            turn_cancelled: false,
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
            resource: None,
            activity: None,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
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
            resource: None,
            activity: None,
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        assert!(m.status_label().0.contains("Waiting"));
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
            resource: None,
            activity: None,
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        // Busy detail is activity-level; lifecycle stays Working.
        assert!(m.status_label().0.contains("Working"));
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Working);
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
            resource: None,
            activity: None,
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
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
            resource: Some("src/app.rs".into()),
            activity: Some("2 changes · Review".into()),
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
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
            resource: None,
            activity: None,
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        assert_eq!(m.current_state_label(), "Waiting");
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
            resource: Some("src/app.rs".into()),
            activity: Some("2 changes · Review".into()),
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Forge"));
        assert!(rendered.contains("forge/main*"));
        assert!(rendered.contains("src/app.rs"));
        assert!(rendered.contains("2 changes"));
        assert!(!rendered.contains("32% context"));
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
            resource: None,
            activity: Some("2 changes".into()),
            turn_cancelled: false,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        let area = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Forge"));
        assert!(rendered.contains("Ready"));
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
        // Busy + Running => Working (tool names stay activity-level, not lifecycle).
        assert_eq!(
            status_model(SessionStatus::Running, true, BusyPhase::Idle).current_state_label(),
            "Working"
        );
        assert_eq!(
            status_model(SessionStatus::Running, true, BusyPhase::Connect).current_state_label(),
            "Waiting"
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
            "Working"
        );
        assert_eq!(
            status_model(
                SessionStatus::Running,
                true,
                BusyPhase::Tool { name: "git".into() }
            )
            .current_state_label(),
            "Working"
        );
        assert_eq!(
            status_model(SessionStatus::Running, false, BusyPhase::Idle).current_state_label(),
            "Ready"
        );
        let mut cancelled = status_model(SessionStatus::Running, false, BusyPhase::Idle);
        cancelled.turn_cancelled = true;
        assert_eq!(cancelled.current_state_label(), "Cancelled");
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
            .contains("Working"));
        assert!(busy
            .status_label_with_busy_detail(Some("custom detail"))
            .0
            .contains("custom detail"));
        assert!(busy
            .status_label_with_busy_detail(Some("custom detail"))
            .0
            .contains("Working"));

        assert!(status_model(SessionStatus::Running, false, BusyPhase::Idle)
            .status_label()
            .0
            .contains("Ready"));
        assert!(
            status_model(SessionStatus::Completed, false, BusyPhase::Idle)
                .status_label()
                .0
                .contains("Completed")
        );
        assert!(status_model(SessionStatus::Failed, false, BusyPhase::Idle)
            .status_label()
            .0
            .contains("Failed"));

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

    #[test]
    fn ready_when_no_active_task() {
        let m = status_model(SessionStatus::Running, false, BusyPhase::Idle);
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Ready);
        assert_eq!(m.current_state_label(), "Ready");
        assert!(m.status_label().0.contains("Ready"));
    }

    #[test]
    fn working_with_typed_progress_updates_in_place() {
        let mut m = status_model(
            SessionStatus::Running,
            true,
            BusyPhase::Tool {
                name: "fffind".into(),
            },
        );
        m.progress_description = Some("Searching files".into());
        let label = m.status_label().0;
        assert!(label.contains("Working"), "{label}");
        assert!(label.contains("Searching files"), "{label}");
        m.progress_description = Some("Reading README".into());
        m.busy_phase = BusyPhase::Tool {
            name: "read_file".into(),
        };
        let label = m.status_label().0;
        assert!(label.contains("Reading README"), "{label}");
        assert!(!label.contains("Searching files"), "{label}");
    }

    #[test]
    fn waiting_for_approval_and_input() {
        let m = status_model(SessionStatus::AwaitingHitl, false, BusyPhase::Idle);
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Waiting);
        assert!(m.status_label().0.contains("Waiting"));
        assert!(m.status_label().0.contains("Approval required"));

        let mut input = status_model(SessionStatus::Running, true, BusyPhase::Connect);
        assert_eq!(input.turn_lifecycle(), TurnLifecycle::Waiting);
        assert!(input.status_label().0.contains("Your input required"));
        input.busy = false;
        input.busy_phase = BusyPhase::Idle;
        input.waiting_detail = Some("Your input required".into());
        assert_eq!(input.turn_lifecycle(), TurnLifecycle::Waiting);
    }

    #[test]
    fn terminal_states_map_structurally() {
        assert_eq!(
            status_model(SessionStatus::Completed, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Completed
        );
        assert_eq!(
            status_model(SessionStatus::Failed, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Failed
        );
        assert_eq!(
            status_model(SessionStatus::Cancelled, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Cancelled
        );
        assert_eq!(
            status_model(SessionStatus::Interrupted, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Interrupted
        );
        let mut failed = status_model(SessionStatus::Failed, false, BusyPhase::Idle);
        failed.failure_category = Some("Tool retries exhausted".into());
        let label = failed.status_label().0;
        assert!(label.contains("Failed"), "{label}");
        assert!(label.contains("Tool retries exhausted"), "{label}");
        assert!(!label.contains("validation"), "{label}");
    }

    #[test]
    fn child_activity_failure_does_not_fail_header() {
        let mut m = status_model(
            SessionStatus::Running,
            true,
            BusyPhase::Other("Trying another approach".into()),
        );
        m.activity = Some("✗ read_file failed".into());
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Working);
        assert!(m.status_label().0.contains("Working"));
        assert!(!m.status_label().0.contains("Failed"));
    }

    #[test]
    fn narrow_width_keeps_state_label() {
        let mut m = status_model(
            SessionStatus::Running,
            true,
            BusyPhase::Tool {
                name: "read_file".into(),
            },
        );
        m.repo_name = Some("very-long-repository-name".into());
        m.branch = Some("feature/extremely-long-branch-name".into());
        m.progress_description = Some("Inspecting repository".into());
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Working"), "{rendered}");

        let area = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Working"), "{rendered}");
        // Progress may be dropped; state remains intact.
        assert!(!rendered.contains("Inspecting repository") || rendered.contains("Working"));
    }

    #[test]
    fn cancelled_flag_and_durable_status_agree() {
        let durable = status_model(SessionStatus::Cancelled, false, BusyPhase::Idle);
        assert_eq!(durable.current_state_label(), "Cancelled");
        let mut flag = status_model(SessionStatus::Running, false, BusyPhase::Idle);
        flag.turn_cancelled = true;
        assert_eq!(flag.current_state_label(), "Cancelled");
    }
}
