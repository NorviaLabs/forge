//! Session chrome data for `/status`.

use crate::status_glyph::{status_glyph, Status};
use crate::theme;
use forge_types::TaskLifecycle;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use std::path::Path;

/// Collapse a home-directory prefix to `~` for the top-bar identity line.
/// No further truncation — a home-relative path is the only case the
/// design covers today.
pub fn shorten_home_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() {
                "~".to_string()
            } else {
                format!("~/{}", rest.display())
            };
        }
    }
    path.display().to_string()
}

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
        "ls" => "Listing files".into(),
        "view_image" => "Viewing image".into(),
        "write_file" | "apply_patch" => "Editing files".into(),
        "glob" => "Searching files".into(),
        "grep" | "rg" => "Searching code".into(),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TurnLifecycle {
    /// No active turn; ready for input.
    #[default]
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
    /// Direct, 1:1 mirror of the authoritative `TaskLifecycle` — kept as a
    /// distinct type only because it carries rendering-specific methods
    /// (`.style()`, `.symbol()`, `.label()`); it must never re-derive its
    /// value from `busy`/cancel flags or transcript content.
    pub fn from_task_lifecycle(status: TaskLifecycle) -> Self {
        match status {
            TaskLifecycle::Ready => Self::Ready,
            TaskLifecycle::Working => Self::Working,
            TaskLifecycle::Waiting => Self::Waiting,
            TaskLifecycle::Failed => Self::Failed,
            TaskLifecycle::Completed => Self::Completed,
            TaskLifecycle::Cancelled => Self::Cancelled,
            TaskLifecycle::Interrupted => Self::Interrupted,
            // `TaskLifecycle` is `#[non_exhaustive]`. Never render an unrecognised status as
            // Ready or Working — that would imply a live runtime we cannot confirm.
            _ => Self::Interrupted,
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
            Self::Working => theme::agent().add_modifier(Modifier::BOLD),
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

fn format_lifecycle_label(life: TurnLifecycle, detail: Option<&str>, _animated: bool) -> String {
    let glyph = life.symbol();
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
    pub status: TaskLifecycle,
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
    /// Vendor display name for `connect_profile`, e.g. "OpenAI" — `None`
    /// until a connect profile is resolved.
    pub vendor_label: Option<String>,
    /// This profile's offering label, e.g. "ChatGPT sign-in" — only set when
    /// its vendor has more than one registered route.
    pub route_label: Option<String>,
    pub web_search_label: Option<String>,
    pub tools_visible: usize,
    pub prompt_cache_hits: u64,
    pub prompt_cache_writes: u64,
    pub repo_name: Option<String>,
    pub branch: Option<String>,
    pub dirty: bool,
    /// Shortened working-directory path for the top-bar identity line
    /// (home-dir prefix collapsed to `~`) — see [`shorten_home_path`].
    pub cwd_display: String,
    pub resource: Option<String>,
    pub activity: Option<String>,
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

    /// Overall turn lifecycle label (not tool/activity phase). A direct,
    /// non-inferred mapping of the authoritative session lifecycle — the
    /// one legitimate override is connecting to a provider, which isn't
    /// itself a task and so has no `TaskLifecycle` equivalent.
    pub fn turn_lifecycle(&self) -> TurnLifecycle {
        if self.busy && matches!(self.busy_phase, BusyPhase::Connect) {
            return TurnLifecycle::Waiting;
        }
        TurnLifecycle::from_task_lifecycle(self.status)
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

    /// Top-bar identity line: `⌂ path  ·  ⎇ branch*`. Path is always
    /// shown (falls back to `cwd_display` even with no git repo); branch
    /// is omitted when there isn't one.
    pub fn identity_line(&self) -> String {
        let mut line = format!("⌂ {}", self.cwd_display);
        if let Some(branch) = self.branch.as_deref().filter(|b| !b.is_empty()) {
            line.push_str("  ·  ⎇ ");
            line.push_str(branch);
            if self.dirty {
                line.push('*');
            }
        }
        line
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
                    } else if self.status == TaskLifecycle::Waiting {
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

    #[allow(dead_code)]
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
        // Centered single block: ⌂ path  ·  ⎇ branch — identity only,
        // full window width, changes only on project/branch switch.
        let content = self.model.identity_line();

        let width = area.width as usize;
        let content_width = content.chars().count();

        theme::fill(area, buf, theme::status_bar());
        if content_width <= width {
            let pad = (width.saturating_sub(content_width)) / 2;
            let padded = format!("{}{}", " ".repeat(pad), content);
            buf.set_line(area.x, area.y, &Line::from(padded), area.width);
        } else {
            // Too wide: left-align the Forge line
            buf.set_line(area.x, area.y, &Line::from(content), area.width);
        }
    }
}

#[allow(dead_code)]
fn push_lifecycle_label(
    spans: &mut Vec<Span<'static>>,
    life: TurnLifecycle,
    label: &str,
    style: ratatui::style::Style,
) {
    let status = match life {
        TurnLifecycle::Completed => Some(Status::Success),
        TurnLifecycle::Failed => Some(Status::Error),
        _ => None,
    };
    if let Some(status) = status {
        if let Some(rest) = label
            .strip_prefix(life.symbol())
            .and_then(|text| text.strip_prefix(' '))
        {
            spans.push(status_glyph(status));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(rest.to_string(), style));
            return;
        }
    }
    spans.push(Span::styled(label.to_string(), style));
}

/// One formatting rule for the model picker's active selection.
pub fn format_provider_model_effort(
    vendor_label: &str,
    route_label: Option<&str>,
    model: &str,
    effort: &str,
) -> String {
    match route_label {
        Some(route) => format!("{vendor_label} · {route} / {model} / {effort}"),
        None => format!("{vendor_label} / {model} / {effort}"),
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

    #[test]
    fn shortens_home_prefix_to_tilde() {
        let _lock = crate::app::tests::helpers::lock_test_env();
        if let Some(home) = dirs::home_dir() {
            let path = home.join("Projects/forge");
            assert_eq!(shorten_home_path(&path), "~/Projects/forge");
            assert_eq!(shorten_home_path(&home), "~");
        }
    }

    #[test]
    fn leaves_non_home_paths_unshortened() {
        assert_eq!(shorten_home_path(Path::new("/opt/other")), "/opt/other");
    }

    #[test]
    fn identity_line_omits_branch_when_absent() {
        let mut m = status_model(TaskLifecycle::Ready, false, BusyPhase::Idle);
        m.cwd_display = "~/Projects/forge".into();
        m.branch = None;
        assert_eq!(m.identity_line(), "⌂ ~/Projects/forge");
    }

    #[test]
    fn identity_line_appends_dirty_marker() {
        let mut m = status_model(TaskLifecycle::Ready, false, BusyPhase::Idle);
        m.cwd_display = "~/Projects/forge".into();
        m.branch = Some("main".into());
        m.dirty = true;
        assert_eq!(m.identity_line(), "⌂ ~/Projects/forge  ·  ⎇ main*");
    }

    fn status_model(status: TaskLifecycle, busy: bool, busy_phase: BusyPhase) -> StatusModel {
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
            vendor_label: None,
            route_label: None,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
            cwd_display: "~/Projects/forge".to_string(),
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
            status: TaskLifecycle::Waiting,
            session_short: "abcd".into(),
            model: "mock".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.1,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            vendor_label: None,
            route_label: None,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
            cwd_display: "~/Projects/forge".to_string(),
            resource: None,
            activity: None,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        assert!(m.status_label().0.contains("Waiting"));
    }

    #[test]
    fn busy_phase_model() {
        let m = StatusModel {
            status: TaskLifecycle::Working,
            session_short: "x".into(),
            model: "m".into(),
            provider: "native".into(),
            effort: "high".into(),
            ctx_pct: 0.0,
            busy: true,
            busy_phase: BusyPhase::Model,
            connect_profile: None,
            provider_connected: false,
            vendor_label: None,
            route_label: None,
            web_search_label: Some("mock".into()),
            tools_visible: 5,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: None,
            branch: None,
            dirty: false,
            cwd_display: "~/Projects/forge".to_string(),
            resource: None,
            activity: None,
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
            status: TaskLifecycle::Working,
            session_short: "abc".into(),
            model: "openai/gpt".into(),
            provider: "native".into(),
            effort: "medium".into(),
            ctx_pct: 0.34,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: Some("xai".into()),
            provider_connected: true,
            vendor_label: None,
            route_label: None,
            web_search_label: Some("mock".into()),
            tools_visible: 4,
            prompt_cache_hits: 2,
            prompt_cache_writes: 1,
            repo_name: None,
            branch: None,
            dirty: false,
            cwd_display: "~/Projects/forge".to_string(),
            resource: None,
            activity: None,
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
            status: TaskLifecycle::Working,
            session_short: "abc".into(),
            model: "openai-code/gpt-5.4".into(),
            provider: "native".into(),
            effort: "medium".into(),
            ctx_pct: 0.34,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: Some("openai-code".into()),
            provider_connected: true,
            vendor_label: None,
            route_label: None,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: true,
            cwd_display: "~/Projects/forge".to_string(),
            resource: Some("src/app.rs".into()),
            activity: Some("2 changes".into()),
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
            status: TaskLifecycle::Waiting,
            session_short: "abc".into(),
            model: "mock".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.32,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            vendor_label: None,
            route_label: None,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: false,
            cwd_display: "~/Projects/forge".to_string(),
            resource: None,
            activity: None,
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        assert_eq!(m.current_state_label(), "Waiting");
    }

    #[test]
    fn status_bar_renders_full_header_when_wide() {
        let m = StatusModel {
            status: TaskLifecycle::Working,
            session_short: "abc".into(),
            model: "gpt-5.6-terra".into(),
            provider: "mock".into(),
            effort: "medium".into(),
            ctx_pct: 0.32,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            vendor_label: Some("OpenAI".into()),
            route_label: Some("ChatGPT sign-in".into()),
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: true,
            cwd_display: "~/Projects/forge".to_string(),
            resource: Some("src/app.rs".into()),
            activity: Some("2 changes".into()),
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        let area = Rect::new(0, 0, 80, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        // Identity-only centered line: directory + branch, no status.
        assert!(rendered.contains("~/Projects/forge"));
        assert!(rendered.contains("main*"));
    }

    #[test]
    fn status_bar_preserves_identity_on_narrow_width() {
        let m = StatusModel {
            status: TaskLifecycle::Ready,
            session_short: "abc".into(),
            model: "very-long-model-name".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.32,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            provider_connected: true,
            vendor_label: None,
            route_label: None,
            web_search_label: None,
            tools_visible: 0,
            prompt_cache_hits: 0,
            prompt_cache_writes: 0,
            repo_name: Some("forge".into()),
            branch: Some("main".into()),
            dirty: false,
            cwd_display: "~/Projects/forge".to_string(),
            resource: None,
            activity: Some("2 changes".into()),
            progress_description: None,
            failure_category: None,
            waiting_detail: None,
        };
        let area = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        // Narrow width falls back to left-aligned rather than dropping content.
        assert!(rendered.contains("Projects/forge") || rendered.contains("main"));
    }

    #[test]
    fn current_state_label_covers_status_and_busy_phase_branches() {
        assert_eq!(
            status_model(TaskLifecycle::Completed, false, BusyPhase::Idle).current_state_label(),
            "Completed"
        );
        assert_eq!(
            status_model(TaskLifecycle::Failed, false, BusyPhase::Idle).current_state_label(),
            "Failed"
        );
        // Busy + Running => Working (tool names stay activity-level, not lifecycle).
        assert_eq!(
            status_model(TaskLifecycle::Working, true, BusyPhase::Idle).current_state_label(),
            "Working"
        );
        assert_eq!(
            status_model(TaskLifecycle::Working, true, BusyPhase::Connect).current_state_label(),
            "Waiting"
        );
        assert_eq!(
            status_model(
                TaskLifecycle::Working,
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
                TaskLifecycle::Working,
                true,
                BusyPhase::Tool { name: "git".into() }
            )
            .current_state_label(),
            "Working"
        );
        assert_eq!(
            status_model(TaskLifecycle::Ready, false, BusyPhase::Idle).current_state_label(),
            "Ready"
        );
        assert_eq!(
            status_model(TaskLifecycle::Cancelled, false, BusyPhase::Idle).current_state_label(),
            "Cancelled"
        );
        assert_eq!(BusyPhase::Tool { name: "git".into() }.label(), "tool:git");
        assert_eq!(BusyPhase::Connect.label(), "connect");
        assert_eq!(BusyPhase::Other("phase".into()).label(), "phase");
    }

    #[test]
    fn status_label_and_truncation_cover_edge_cases() {
        let busy = status_model(
            TaskLifecycle::Working,
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

        assert!(status_model(TaskLifecycle::Ready, false, BusyPhase::Idle)
            .status_label()
            .0
            .contains("Ready"));
        assert!(
            status_model(TaskLifecycle::Completed, false, BusyPhase::Idle)
                .status_label()
                .0
                .contains("Completed")
        );
        assert!(status_model(TaskLifecycle::Failed, false, BusyPhase::Idle)
            .status_label()
            .0
            .contains("Failed"));

        assert_eq!(StatusModel::truncate_model("abcdef", 3), "abc");
        assert_eq!(StatusModel::truncate_middle("abcdef", 4), "abcd");
        assert_eq!(StatusModel::truncate_middle("abcdef", 5), "ab…ef");

        let mut m = status_model(TaskLifecycle::Ready, false, BusyPhase::Idle);
        assert_eq!(m.repo_branch_label(), None);
        m.repo_name = Some("forge".into());
        assert_eq!(m.repo_branch_label().as_deref(), Some("forge"));
    }

    #[test]
    fn status_bar_handles_zero_sized_area() {
        let m = status_model(TaskLifecycle::Ready, false, BusyPhase::Idle);
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
    }

    #[test]
    fn ready_when_no_active_task() {
        let m = status_model(TaskLifecycle::Ready, false, BusyPhase::Idle);
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Ready);
        assert_eq!(m.current_state_label(), "Ready");
        assert!(m.status_label().0.contains("Ready"));
    }

    #[test]
    fn working_with_typed_progress_updates_in_place() {
        let mut m = status_model(
            TaskLifecycle::Working,
            true,
            BusyPhase::Tool {
                name: "glob".into(),
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
        let m = status_model(TaskLifecycle::Waiting, false, BusyPhase::Idle);
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Waiting);
        assert!(m.status_label().0.contains("Waiting"));
        assert!(m.status_label().0.contains("Approval required"));

        let input = status_model(TaskLifecycle::Working, true, BusyPhase::Connect);
        assert_eq!(input.turn_lifecycle(), TurnLifecycle::Waiting);
        assert!(input.status_label().0.contains("Your input required"));
    }

    #[test]
    fn waiting_detail_alone_does_not_override_an_authoritative_non_waiting_status() {
        // `waiting_detail` is presentation-only text; it must never itself
        // flip the header lifecycle away from what `status` (the
        // authoritative session lifecycle) says — that would resurrect
        // exactly the kind of ad hoc inference this type exists to prevent.
        let mut m = status_model(TaskLifecycle::Working, false, BusyPhase::Idle);
        m.waiting_detail = Some("Your input required".into());
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Working);
    }

    #[test]
    fn terminal_states_map_structurally() {
        assert_eq!(
            status_model(TaskLifecycle::Completed, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Completed
        );
        assert_eq!(
            status_model(TaskLifecycle::Failed, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Failed
        );
        assert_eq!(
            status_model(TaskLifecycle::Cancelled, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Cancelled
        );
        assert_eq!(
            status_model(TaskLifecycle::Interrupted, false, BusyPhase::Idle).turn_lifecycle(),
            TurnLifecycle::Interrupted
        );
        let mut failed = status_model(TaskLifecycle::Failed, false, BusyPhase::Idle);
        failed.failure_category = Some("Tool retries exhausted".into());
        let label = failed.status_label().0;
        assert!(label.contains("Failed"), "{label}");
        assert!(label.contains("Tool retries exhausted"), "{label}");
        assert!(!label.contains("validation"), "{label}");
    }

    #[test]
    fn child_activity_failure_does_not_fail_header() {
        let mut m = status_model(
            TaskLifecycle::Working,
            true,
            BusyPhase::Other("Trying another approach".into()),
        );
        m.activity = Some("✗ read_file failed".into());
        assert_eq!(m.turn_lifecycle(), TurnLifecycle::Working);
        assert!(m.status_label().0.contains("Working"));
        assert!(!m.status_label().0.contains("Failed"));
    }

    #[test]
    fn narrow_width_keeps_identity_line() {
        let mut m = status_model(
            TaskLifecycle::Working,
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
        // Status removed from the top bar; it's identity-only now.
        assert!(rendered.contains("Projects/forge"), "{rendered}");

        let area = Rect::new(0, 0, 24, 1);
        let mut buf = Buffer::empty(area);
        StatusBar { model: &m }.render(area, &mut buf);
        let rendered: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(rendered.contains("Projects/forge"), "{rendered}");
    }

    #[test]
    fn cancelled_status_is_authoritative_regardless_of_busy() {
        // There is no separate "cancelled" flag to disagree with `status`
        // anymore — `TaskLifecycle::Cancelled` alone is the single source
        // of truth, whether or not a stale `busy` flag lingers.
        let durable = status_model(TaskLifecycle::Cancelled, false, BusyPhase::Idle);
        assert_eq!(durable.current_state_label(), "Cancelled");
        let durable_busy = status_model(TaskLifecycle::Cancelled, true, BusyPhase::Idle);
        assert_eq!(durable_busy.current_state_label(), "Cancelled");
    }
}
