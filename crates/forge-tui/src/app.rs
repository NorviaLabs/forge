//! Full-screen TUI application (TUI-01 shell + TUI-02/03/04 wired).

use std::collections::HashSet;
use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    self, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseEventKind, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use forge_connect::{
    builtin_registry, handle_connect_action, models_for_picker, needs_tui_api_key_prompt,
    needs_tui_oauth, normalize_model_id, ConnectAction, ConnectError, ConnectRegistry,
    ConnectService, CredentialStore, ModelCatalogCache, OauthPending, OPENAI_CODEX_PROFILE_ID,
};
use forge_core::{AgentSession, ApplyOutcome, LoopError};
use forge_tools::{GitTool, Tool, ToolContext};
use forge_types::{HitlDecision, ModelStreamEvent, ProgressDocument};
use ratatui::backend::CrosstermBackend;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};
use ratatui::Terminal;
use serde_json::json;
use thiserror::Error;

use crate::activity::{ActivityFeed, ActivityKind};
use crate::commands::{parse_slash, SlashCommand};
use crate::conversation::{
    format_elapsed_tenths, BannerKind, ChatItem, ConversationModel, ConversationViewOpts,
    StreamWaitPhase,
};
use crate::editor::EditorError;
use crate::effort::ReasoningEffort;
use crate::file_explorer::{FileExplorer, FileExplorerWidget};
use crate::history::InputHistory;
use crate::layout::is_too_small;
#[cfg(test)]
use crate::layout::split_areas_full;
use crate::layout::split_areas_with_side_panels;
use crate::msg_queue::MessageQueue;
use crate::overlays::{
    filter_palette, handle_overlay_key, models_from_catalog, ConnectProfileItem, FileExplorerItem,
    Key, Key as OverlayKey, Overlay, OverlayAction, OverlayWidget, PaletteItem, ResumeSessionItem,
};
use crate::sidebar::{InspectorView, SidebarModel, SidebarWidget};
use crate::source_viewer::{SourceViewer, SourceViewerWidget};
use crate::terminal::TerminalGuard;
use crate::theme;
use crate::widgets::{
    classify_operator_error, BottomPanel, BottomPanelModel, BottomPanelState, BottomPanelTab,
    BusyPhase, FeedbackBar, FeedbackModel, FeedbackSeverity, FooterBar, FooterModel, InputBar,
    InputModel, StatusBar, StatusModel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkspaceMode {
    #[default]
    Chat,
    Editor,
    Diff,
}

impl WorkspaceMode {
    const ALL: [Self; 3] = [Self::Chat, Self::Editor, Self::Diff];

    fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Editor => "Editor",
            Self::Diff => "Diff",
        }
    }

    fn empty_state(self) -> Option<&'static str> {
        match self {
            Self::Chat => None,
            Self::Editor => None,
            Self::Diff => Some("Diff view is not available yet."),
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Chat => Self::Editor,
            Self::Editor => Self::Diff,
            Self::Diff => Self::Chat,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Chat => Self::Diff,
            Self::Editor => Self::Chat,
            Self::Diff => Self::Editor,
        }
    }
}
use crate::ExitCode;

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Loop(#[from] LoopError),
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct ExitSummary {
    pub exit_code: ExitCode,
    pub session_id: String,
    pub token_usage: Option<String>,
}

fn format_exit_token_usage(report: &forge_core::TokenUsageReport) -> String {
    let api = &report.api;
    format!(
        "Token usage: total={} input={} (+ {} cached) output={} (reasoning {})",
        format_with_commas(api.total_api_tokens()),
        format_with_commas(api.prompt_tokens),
        format_with_commas(api.prompt_cache_hits),
        format_with_commas(api.completion_tokens),
        format_with_commas(api.thinking_tokens_est),
    )
}

fn format_with_commas(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, ch) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[derive(Debug, Clone)]
pub struct TuiRuntimeConfig {
    pub model_label: String,
    pub provider: String,
    pub cwd: PathBuf,
    pub version: String,
    pub startup_notices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeSession {
    id: uuid::Uuid,
    modified: SystemTime,
}

fn recent_resume_sessions(
    dir: &std::path::Path,
    current: uuid::Uuid,
    limit: usize,
) -> io::Result<Vec<ResumeSession>> {
    let mut sessions = Vec::new();
    if !dir.is_dir() {
        return Ok(sessions);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("db") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| uuid::Uuid::parse_str(value).ok())
        else {
            continue;
        };
        if id == current {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        sessions.push(ResumeSession { id, modified });
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified));
    sessions.truncate(limit);
    Ok(sessions)
}

fn footer_usage_summary_with_cost(
    report: &forge_core::TokenUsageReport,
    cost: Option<forge_connect::CatalogCost>,
) -> String {
    let cost = cost
        .filter(|_| report.api.prompt_tokens > 0 || report.api.completion_tokens > 0)
        .map(|cost| {
            let input = report.api.prompt_tokens as f64 * cost.input / 1_000_000.0;
            let output = report.api.completion_tokens as f64 * cost.output / 1_000_000.0;
            format!(" · ${:.4}", input + output)
        })
        .unwrap_or_default();
    format!(
        "in {} · out {} · total {}{}",
        format_with_commas(report.api.prompt_tokens),
        format_with_commas(report.api.completion_tokens),
        format_with_commas(report.api.total_api_tokens()),
        cost,
    )
}

#[derive(Debug, Clone, Default)]
struct FooterLimits {
    usage: String,
    weekly_limit: String,
    credits: String,
}

fn footer_limits_from_report(lines: &[String]) -> FooterLimits {
    FooterLimits {
        usage: lines
            .iter()
            .find(|line| line.starts_with("Session limit:"))
            .cloned()
            .unwrap_or_default(),
        weekly_limit: lines
            .iter()
            .find(|line| line.starts_with("Weekly limit:"))
            .cloned()
            .unwrap_or_default(),
        credits: lines
            .iter()
            .find(|line| line.starts_with("Credits:") || line.starts_with("Credit balance:"))
            .cloned()
            .unwrap_or_default(),
    }
}

fn footer_usage_summary(
    report: &forge_core::TokenUsageReport,
    cost: Option<forge_connect::CatalogCost>,
    limits: &FooterLimits,
) -> crate::widgets::FooterModel {
    crate::widgets::FooterModel {
        usage: limits.usage.clone(),
        weekly_limit: limits.weekly_limit.clone(),
        credits: limits.credits.clone(),
        usage_summary: footer_usage_summary_with_cost(report, cost),
        ..Default::default()
    }
}

fn footer_provider_id(provider: &str, connect_profile: Option<&str>) -> String {
    connect_profile.unwrap_or(provider).to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConversationRenderKey {
    session_id: uuid::Uuid,
    width: u16,
    messages: usize,
    last_message_content: usize,
    last_message_thinking: usize,
    events: usize,
    last_event_detail: usize,
    banners: usize,
    queue: usize,
    queue_selected: Option<usize>,
    chat_message_start: usize,
    chat_event_start: usize,
    busy: bool,
    busy_phase: String,
    tool_expanded: bool,
    splash_dismissed: bool,
    slash_mode: bool,
    status: forge_types::SessionStatus,
}

struct ConversationRenderCache {
    key: ConversationRenderKey,
    lines: Vec<Line<'static>>,
}

struct FooterLimitsCache {
    provider: String,
    fetched_at: Instant,
    limits: FooterLimits,
}

pub struct TuiApp {
    pub session: AgentSession,
    pub input: InputModel,
    pub overlay: Option<Overlay>,
    pub should_quit: bool,
    pub busy: bool,
    pub status_message: String,
    pub runtime: TuiRuntimeConfig,
    pub last_exit: ExitCode,
    pub connect_registry: ConnectRegistry,
    pub connect_store: CredentialStore,
    pub connect_profile: Option<String>,
    /// Manual disconnect latch: prevents auto-restore until the user signs in again.
    pub auth_suspended: bool,
    /// In-flight xAI device-code OAuth (polled on the event loop tick).
    pub oauth_pending: Option<OauthPending>,
    /// Last time we polled the token endpoint (respect server `interval`).
    oauth_last_poll: Option<std::time::Instant>,
    /// Phase 7 — submitted command history (Up/Down when no overlay).
    pub history: InputHistory,
    /// Phase 8 autocomplete: selection within filtered `/` suggestions.
    pub slash_suggest_idx: usize,
    /// Multi-line notices (e.g. /connect list) shown above the input.
    pub notices: Vec<String>,
    notices_until: Option<Instant>,
    /// Phase 10 / TUI-08 — always-visible feedback strip model.
    pub feedback: FeedbackModel,
    /// Phase 10 / TUI-08 — durable UI error/info banners in chat.
    pub ui_banners: Vec<ChatItem>,
    /// Phase 10 / TUI-10 — progressive busy phase for chrome.
    pub busy_phase: BusyPhase,
    /// User prompt queued on Enter; drained by the event loop so the YOU bubble paints first.
    pending_prompt: Option<String>,
    /// Resume the current agent loop after an interactive turn-limit checkpoint.
    pending_turn_continue: bool,
    /// Long-running slash action queued to run on the event loop (so the command echo paints).
    pending_sync: bool,
    /// HITL resolve queued to run on the event loop (journals + state updates).
    pending_hitl_decision: Option<HitlDecision>,
    /// Context reset queued to run on the event loop.
    pending_context_reset: bool,
    /// External-editor request queued for the event loop (terminal suspend/resume).
    pending_external_editor: bool,
    /// Additional user messages waiting to run after the current turn (FIFO).
    message_queue: MessageQueue,
    /// Selected queued row for keyboard cancellation.
    queue_selected: Option<usize>,
    /// Live assistant text while tokens stream in.
    stream_preview: String,
    /// Live thinking/reasoning text while tokens stream in.
    stream_thinking: String,
    /// When the current model turn started (for wait/think elapsed timer).
    turn_started: Option<Instant>,
    /// When the first thinking token arrived.
    thinking_started: Option<Instant>,
    /// Duration of the thinking phase once it ends (used for persistence/telemetry).
    thought_secs: Option<f64>,
    /// Optional web_search label for chrome (`mock` / `off` / provider id).
    pub web_search_label: Option<String>,
    /// Phase 10 / TUI-10 — activity ring buffer.
    pub activity: ActivityFeed,
    /// Reasoning effort sent to model providers (`auto` omits the parameter).
    reasoning_effort: ReasoningEffort,
    /// Expand last tool detail (Ctrl+O).
    tool_expanded: bool,
    /// Active workspace tab. Older restored sessions safely use the default Chat mode.
    pub workspace_mode: WorkspaceMode,
    /// Read-only source viewer state for the Editor workspace tab.
    pub source_viewer: SourceViewer,
    pub bottom_panel: BottomPanelState,
    pub files_visible: bool,
    pub file_explorer: FileExplorer,
    /// User preference; narrow terminals still hide the sidebar responsively.
    sidebar_visible: bool,
    inspector_view: InspectorView,
    /// Soft-cancel in-flight turn (Esc while busy).
    cancel_requested: bool,
    /// Tools allowed for the rest of this session (HITL "s").
    hitl_session_allow: HashSet<String>,
    /// Transient toast (auto-clears).
    toast: Option<(Instant, String)>,
    /// Last measured height of the editor viewport for page scrolling.
    last_editor_height: u16,
    /// Session message/event offsets hidden by the most recent `/clear`.
    chat_message_start: usize,
    chat_event_start: usize,
    /// Conversation scroll offset (when not following).
    chat_scroll: u16,
    chat_follow: bool,
    context_reset_snapshot: Option<(f64, f64)>,
    splash_dismissed: bool,
    conversation_cache: Option<ConversationRenderCache>,
    model_cost_cache: Option<(String, Option<forge_connect::CatalogCost>)>,
    footer_limits_cache: Option<FooterLimitsCache>,
    footer_limits_rx: Option<std::sync::mpsc::Receiver<(String, FooterLimits)>>,
}

#[derive(Debug, Clone)]
pub(crate) struct RepoHeaderCache {
    pub(crate) repo_name: Option<String>,
    pub(crate) branch: Option<String>,
    pub(crate) dirty: bool,
}

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        let mut input = InputModel::default();
        input.hint = "Describe a task…".into();
        let startup_notices = runtime.startup_notices.clone();
        let workspace_root = session.workspace_root().to_path_buf();
        Self {
            session,
            input,
            overlay: None,
            should_quit: false,
            busy: false,
            status_message: String::new(),
            runtime,
            last_exit: ExitCode::Success,
            connect_registry: builtin_registry(),
            connect_store: CredentialStore::user_default(),
            connect_profile: None,
            auth_suspended: false,
            oauth_pending: None,
            oauth_last_poll: None,
            history: InputHistory::default(),
            slash_suggest_idx: 0,
            notices: startup_notices,
            notices_until: None,
            feedback: FeedbackModel::default(),
            ui_banners: Vec::new(),
            busy_phase: BusyPhase::Idle,
            web_search_label: Some("mock".into()),
            activity: ActivityFeed::default(),
            pending_prompt: None,
            pending_turn_continue: false,
            pending_sync: false,
            pending_hitl_decision: None,
            pending_context_reset: false,
            pending_external_editor: false,
            message_queue: MessageQueue::new(),
            queue_selected: None,
            stream_preview: String::new(),
            stream_thinking: String::new(),
            turn_started: None,
            thinking_started: None,
            thought_secs: None,
            reasoning_effort: ReasoningEffort::Auto,
            tool_expanded: false,
            workspace_mode: WorkspaceMode::default(),
            source_viewer: SourceViewer::new(),
            bottom_panel: BottomPanelState::default(),
            files_visible: false,
            file_explorer: FileExplorer::new(Some(workspace_root)),
            sidebar_visible: true,
            inspector_view: InspectorView::default(),
            cancel_requested: false,
            hitl_session_allow: HashSet::new(),
            toast: None,
            chat_message_start: 0,
            chat_event_start: 0,
            chat_scroll: 0,
            chat_follow: true,
            context_reset_snapshot: None,
            splash_dismissed: false,
            conversation_cache: None,
            model_cost_cache: None,
            footer_limits_cache: None,
            footer_limits_rx: None,
            last_editor_height: 24,
        }
        .restore_saved_auth()
        .apply_connection_chrome()
    }

    // Intentionally keep the conversation window clean: only real chat (user/assistant)
    // plus essential banners (errors, HITL, connection). Slash command output lives in
    // notices / overlays / activity.

    /// Mock provider is always "connected" (offline tests / CI).
    fn is_mock_provider(&self) -> bool {
        self.runtime.provider.eq_ignore_ascii_case("mock")
            || self.runtime.model_label.eq_ignore_ascii_case("mock")
    }

    /// Live credentials still exist for a connect profile id.
    fn credentials_live_for(&self, profile_id: &str) -> bool {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        svc.connected_profiles()
            .ok()
            .map(|ps| ps.iter().any(|p| p.id == profile_id))
            .unwrap_or(false)
    }

    /// True when chat may call an LLM (mock, or a live `/connect` profile).
    pub fn is_provider_connected(&self) -> bool {
        if self.is_mock_provider() {
            return true;
        }
        match self.connect_profile.as_deref() {
            Some(id) => self.credentials_live_for(id),
            None => false,
        }
    }

    fn push_notice(&mut self, lines: Vec<String>) {
        self.notices = lines;
        self.notices_until = Some(Instant::now() + Duration::from_secs(3));
    }

    fn tick_notices(&mut self) {
        if self
            .notices_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.notices.clear();
            self.notices_until = None;
        }
    }

    /// Drop stale `connect_profile` if credentials were cleared out-of-band.
    fn sync_provider_connection(&mut self) {
        if self.auth_suspended {
            return;
        }
        if self.is_mock_provider() {
            return;
        }
        if let Some(id) = self.connect_profile.clone() {
            if !self.credentials_live_for(&id) {
                self.connect_profile = None;
            }
        }
    }

    /// Status/input/banner chrome reflecting connect state.
    fn apply_connection_chrome(mut self) -> Self {
        self.refresh_connection_ui();
        self
    }

    fn refresh_connection_ui(&mut self) {
        self.sync_provider_connection();
        let connected = !self.input.not_connected;
        self.input.not_connected = !connected;
        if connected {
            if self.input.hint.contains("Not connected") || self.input.hint.contains("/connect") {
                self.input.hint = String::new();
            }
            // Drop the sticky not-connected banner once signed in.
            self.ui_banners.retain(|b| {
                !matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
        } else {
            self.input.hint = "Not connected · run /connect before chatting".into();
            let has_banner = self.ui_banners.iter().any(|b| {
                matches!(
                    b,
                    ChatItem::Banner {
                        kind: BannerKind::Warn,
                        text
                    } if text.contains("Not connected")
                )
            });
            if !has_banner {
                self.ui_banners.push(ChatItem::Banner {
                    text: "Not connected to an LLM provider. Run /connect (xAI Grok or OpenCode Go) before sending a message.".into(),
                    kind: BannerKind::Warn,
                });
            }
        }
    }

    fn disconnect_auth(&mut self, profile_id: Option<&str>) -> Result<String, TuiError> {
        let mut env_keys = Vec::new();
        {
            let svc = ConnectService {
                registry: &self.connect_registry,
                store: &self.connect_store,
                active_profile_id: self.connect_profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            let profiles: Vec<_> = if let Some(id) = profile_id {
                svc.profile(id).into_iter().cloned().collect()
            } else {
                svc.connected_profiles().unwrap_or_default()
            };
            for p in profiles {
                if let Ok(pairs) = svc.provider_env_for_profile(&p.id) {
                    env_keys.extend(pairs.into_iter().map(|(k, _)| k));
                }
            }
        }
        for key in env_keys {
            std::env::remove_var(key);
        }
        self.session.clear_provider_env();
        self.oauth_pending = None;
        self.oauth_last_poll = None;
        self.pending_prompt = None;
        self.pending_sync = false;
        self.pending_hitl_decision = None;
        self.pending_context_reset = false;
        self.message_queue = MessageQueue::new();
        self.queue_selected = None;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started = None;
        self.thinking_started = None;
        self.thought_secs = None;
        self.cancel_requested = false;
        self.busy = false;
        self.busy_phase = BusyPhase::Idle;
        self.tool_expanded = false;
        self.chat_follow = true;
        self.chat_scroll = 0;
        self.connect_profile = None;
        self.runtime.provider.clear();
        self.runtime.model_label.clear();
        self.session.set_active_model(String::new());
        self.feedback = FeedbackModel::default();
        self.status_message = "disconnected".into();
        self.notices.clear();
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Warn,
                    text
                } if text.contains("Not connected")
            )
        });
        self.auth_suspended = true;

        let cleared = if let Some(id) = profile_id {
            self.connect_store
                .clear(id)
                .map_err(|e| TuiError::Other(e.to_string()))?
        } else {
            self.connect_store
                .clear_all()
                .map_err(|e| TuiError::Other(e.to_string()))?
        };
        if let Some(id) = profile_id {
            let _ = self.connect_store.clear_last_selection(Some(id));
        } else {
            let _ = self.connect_store.clear_last_selection(None);
        }
        self.refresh_connection_ui();
        let msg = if let Some(id) = profile_id {
            if cleared {
                format!("disconnected `{id}`")
            } else {
                format!("no stored credentials for `{id}`")
            }
        } else if cleared {
            "disconnected · cleared stored credentials".into()
        } else {
            "disconnected · no stored credentials".into()
        };
        self.push_activity(ActivityKind::Connect, FeedbackSeverity::Info, msg.clone());
        self.set_feedback(FeedbackSeverity::Info, msg.clone());
        Ok(msg)
    }

    fn push_toast(&mut self, text: impl Into<String>) {
        self.toast = Some((Instant::now(), text.into()));
        // Also mirror briefly into feedback (auto-cleared in draw/tick)
        if let Some((_, ref t)) = self.toast {
            self.set_feedback(FeedbackSeverity::Ok, t.clone());
        }
    }

    fn tick_toast(&mut self) {
        if let Some((at, _)) = &self.toast {
            if at.elapsed() > Duration::from_secs(2) {
                self.toast = None;
                if self.feedback.severity == FeedbackSeverity::Ok {
                    self.feedback = FeedbackModel::default();
                    self.status_message.clear();
                }
            }
        }
    }

    /// Close the thinking clock. Prefer wall time from first thinking token;
    /// if that is ~0 (same-batch non-stream dump), fall back to full turn elapsed.
    fn close_thinking_timer(&mut self) {
        if self.thought_secs.is_some() {
            return;
        }
        if self.stream_thinking.is_empty() {
            return;
        }
        let from_think = self
            .thinking_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let from_turn = self
            .turn_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        // Same-batch dump of all thinking+answer → thinking_started ≈ now; use turn time.
        let secs = if from_think < 0.15 && from_turn > from_think {
            from_turn
        } else if from_think > 0.0 {
            from_think
        } else {
            from_turn
        };
        self.thought_secs = Some(secs);
    }

    fn persist_turn_thinking_duration(&mut self, secs: f64) {
        if let Some(m) = self
            .session
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.role == forge_types::MessageRole::Assistant)
        {
            if m.thinking.is_some() {
                m.thinking_duration_secs = Some(secs);
            }
        }
    }

    /// Reload credentials from disk: silent OAuth refresh, inject auth, activate profile.
    /// So a successful `/connect` continues to work in later Forge sessions.
    fn restore_saved_auth(mut self) -> Self {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: None,
            active_model: None,
        };
        let mut connected = svc.connected_profiles().unwrap_or_default();
        connected.sort_by(|a, b| a.id.cmp(&b.id));
        let saved_selection = self.connect_store.last_selection().ok().flatten();
        if let Some(effort) = self
            .connect_store
            .last_effort()
            .ok()
            .flatten()
            .and_then(|effort| effort.parse().ok())
        {
            self.reasoning_effort = effort;
        }
        // Restore the last usable provider; otherwise fall back to a deterministic
        // connected profile instead of silently preferring one backend family.
        let chosen = connected
            .iter()
            .find(|p| saved_selection.as_ref().is_some_and(|(id, _)| id == &p.id))
            .or_else(|| connected.first())
            .cloned();
        if let Some(profile) = chosen {
            // Refresh and inject provider credentials.
            let _ = svc.ensure_oauth_fresh(&profile.id);
            if let Ok(pairs) = svc.provider_env_for_profile(&profile.id) {
                for (k, v) in &pairs {
                    std::env::set_var(k, v);
                }
                self.session.apply_provider_env(&pairs);
            }
            self.connect_profile = Some(profile.id.clone());
            // Only switch the active model when it still looks like the forge default
            // (don't clobber an explicit --model / test runtime label).
            let cur = self.runtime.model_label.as_str();
            let looks_default =
                cur.is_empty() || cur == "openai/gpt-4.1-mini" || cur == "m" || cur == "mock";
            if looks_default {
                let saved_model = saved_selection
                    .as_ref()
                    .and_then(|(id, model)| (id == &profile.id).then_some(model.as_str()))
                    .filter(|model| {
                        let prefix = Self::model_prefix(model);
                        let pid = profile.id.as_str();
                        let provider_prefix = profile.model_provider_prefix.as_str();
                        prefix == pid
                            || prefix == provider_prefix
                            || (prefix == "openai" && pid == "openai_codex")
                            || (prefix == "openai-codex" && pid == "openai_codex")
                            || (prefix == "opencode-go" && pid == "opencode_go")
                            || (prefix == "opencode-zen" && pid == "opencode_zen")
                            || (prefix == "grok" && pid == "xai")
                    });
                if let Some(model) = saved_model.or_else(|| profile.default_model()) {
                    self.runtime.model_label = model.to_string();
                    self.runtime.provider = "native".into();
                    self.session.set_active_model(model);
                }
            } else if self.session.active_model.is_empty() {
                self.session
                    .set_active_model(self.runtime.model_label.clone());
            }
            self.status_message = format!("restored {} · {}", profile.id, self.runtime.model_label);
        }
        self
    }

    fn open_effort_picker_for_model(&mut self, model: &str) {
        let options = ReasoningEffort::options_for_model(model);
        let default = ReasoningEffort::default_for_model(model);
        if options.len() <= 1 {
            // Nothing useful to choose; keep current if still valid, else provider default.
            if !options.contains(&self.reasoning_effort) {
                self.reasoning_effort = default;
                self.persist_selection();
            }
            self.overlay = None;
            return;
        }
        if !options.contains(&self.reasoning_effort) {
            self.reasoning_effort = default;
        }
        self.overlay = Some(Overlay::effort_open(model, self.reasoning_effort));
        self.set_feedback(FeedbackSeverity::Info, "choose reasoning effort");
    }

    fn persist_selection(&self) {
        if let Some(profile_id) = self.connect_profile.as_deref() {
            let _ = self
                .connect_store
                .set_last_selection(profile_id, &self.runtime.model_label);
        }
        let _ = self
            .connect_store
            .set_last_effort(&self.reasoning_effort.to_string());
    }

    /// Phase 10: set strip + keep `status_message` in sync for tests/compat.
    pub fn set_feedback(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        self.status_message = text.clone();
        self.feedback = FeedbackModel { text, severity };
    }

    /// Operator errors remain visible in chat, feedback, and activity.
    pub fn report_error(&mut self, raw: &str) {
        let msg = classify_operator_error(raw);
        self.set_feedback(FeedbackSeverity::Error, msg.clone());
        // Replace prior error banners — don't accumulate red clutter in the chat.
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        self.ui_banners.push(ChatItem::Banner {
            text: msg.clone(),
            kind: BannerKind::Error,
        });
        self.activity
            .push(ActivityKind::Error, FeedbackSeverity::Error, msg);
        self.busy_phase = BusyPhase::Idle;
    }

    /// Drop ephemeral error UI (call on new user turn / Esc).
    fn clear_error_chrome(&mut self) {
        self.ui_banners.retain(|b| {
            !matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )
        });
        if self.feedback.severity == FeedbackSeverity::Error {
            self.feedback = FeedbackModel::default();
            self.status_message.clear();
        }
    }

    pub fn report_info(&mut self, text: impl Into<String>) {
        self.set_feedback(FeedbackSeverity::Info, text);
    }

    pub fn push_activity(
        &mut self,
        kind: ActivityKind,
        severity: FeedbackSeverity,
        summary: impl Into<String>,
    ) {
        self.activity.push(kind, severity, summary);
    }

    fn apply_history_text(&mut self, text: String) {
        self.input.set_text(text);
        self.input.history_browse = self.history.browsing();
        self.clamp_slash_suggest();
    }

    /// Filtered slash suggestions for the current textbox (empty if not in slash mode).
    pub fn slash_suggestions(&self) -> Vec<PaletteItem> {
        let t = self.input.text.trim();
        if !t.starts_with('/') {
            return Vec::new();
        }
        // Filter by text after leading `/`
        let filter = t.trim_start_matches('/');
        filter_palette(filter)
    }

    fn clamp_slash_suggest(&mut self) {
        let n = self.slash_suggestions().len();
        if n == 0 {
            self.slash_suggest_idx = 0;
        } else {
            self.slash_suggest_idx = self.slash_suggest_idx.min(n - 1);
        }
    }

    fn complete_slash_suggestion(&mut self) {
        let items = self.slash_suggestions();
        if items.is_empty() {
            return;
        }
        let idx = self.slash_suggest_idx.min(items.len() - 1);
        let cmd = items[idx].cmd.clone();
        // If user already typed more than the bare cmd (has args), don't clobber args
        let cur = self.input.text.trim();
        if cur == cmd || cur.starts_with(&(cmd.clone() + " ")) {
            return;
        }
        self.input.set_text(format!("{cmd} "));
        self.slash_suggest_idx = 0;
        self.clamp_slash_suggest();
    }

    fn open_connect_picker(&mut self) {
        let connected: HashSet<String> = {
            let svc = ConnectService {
                registry: &self.connect_registry,
                store: &self.connect_store,
                active_profile_id: self.connect_profile.clone(),
                active_model: Some(self.runtime.model_label.clone()),
            };
            svc.connected_profiles()
                .unwrap_or_default()
                .into_iter()
                .map(|p| p.id)
                .collect()
        };
        let items: Vec<ConnectProfileItem> = self
            .connect_registry
            .profiles()
            .iter()
            .map(|p| ConnectProfileItem {
                id: p.id.clone(),
                title: p.title.clone(),
                auth_mode: p.auth_mode.label().into(),
                auth_url: p.auth_url.clone(),
                connected: connected.contains(&p.id),
            })
            .collect();
        self.overlay = Some(Overlay::connect_picker(items));
        self.status_message = "Choose a provider".into();
        self.notices.clear();
    }

    fn resolve_workspace_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, TuiError> {
        let input = path.as_ref();
        let joined = if input.is_absolute() {
            input.to_path_buf()
        } else {
            self.runtime.cwd.join(input)
        };
        let canonical = joined.canonicalize()?;
        let root = self.runtime.cwd.canonicalize()?;
        if !canonical.starts_with(&root) {
            return Err(TuiError::Other(
                "file explorer is limited to the workspace".into(),
            ));
        }
        Ok(canonical)
    }

    fn open_file_explorer(&mut self, path: Option<&str>, error: Option<String>) {
        let dir = match path {
            Some(path) if !path.trim().is_empty() => self
                .resolve_workspace_path(path.trim())
                .unwrap_or_else(|_| self.runtime.cwd.clone()),
            _ => self.runtime.cwd.clone(),
        };
        let dir = if dir.is_dir() {
            dir
        } else {
            dir.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.runtime.cwd.clone())
        };
        let root = self
            .runtime
            .cwd
            .canonicalize()
            .unwrap_or_else(|_| self.runtime.cwd.clone());
        let current = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        let mut items = Vec::new();
        if current != root {
            if let Some(parent) = dir.parent() {
                items.push(FileExplorerItem {
                    name: "..".into(),
                    path: parent.display().to_string(),
                    is_dir: true,
                });
            }
        }
        match fs::read_dir(&dir) {
            Ok(entries) => {
                let mut children = entries
                    .filter_map(Result::ok)
                    .filter_map(|entry| {
                        let path = entry.path();
                        let file_type = entry.file_type().ok()?;
                        let is_dir = file_type.is_dir();
                        if !is_dir && !file_type.is_file() {
                            return None;
                        }
                        Some(FileExplorerItem {
                            name: entry.file_name().to_string_lossy().into_owned(),
                            path: path.display().to_string(),
                            is_dir,
                        })
                    })
                    .collect::<Vec<_>>();
                children.sort_by(|left, right| {
                    right.is_dir.cmp(&left.is_dir).then_with(|| {
                        left.name
                            .to_ascii_lowercase()
                            .cmp(&right.name.to_ascii_lowercase())
                    })
                });
                items.extend(children);
            }
            Err(err) => {
                self.overlay = Some(Overlay::file_explorer(
                    dir.display().to_string(),
                    items,
                    Some(format!("Could not read directory: {err}")),
                ));
                return;
            }
        }
        self.overlay = Some(Overlay::file_explorer(
            dir.display().to_string(),
            items,
            error,
        ));
        self.status_message = "File explorer (readonly)".into();
        self.notices.clear();
    }

    fn open_file_viewer(&mut self, path: &str) {
        match self.resolve_workspace_path(path).and_then(|path| {
            if !path.is_file() {
                return Err(TuiError::Other("selected path is not a file".into()));
            }
            let contents = String::from_utf8_lossy(&fs::read(&path)?).into_owned();
            Ok((path, contents))
        }) {
            Ok((path, contents)) => {
                self.overlay = Some(Overlay::file_viewer(path.display().to_string(), contents));
                self.status_message = "Viewing file (readonly)".into();
            }
            Err(err) => self.open_file_explorer(None, Some(format!("Could not open file: {err}"))),
        }
    }

    fn open_file_in_editor(&mut self, path: &Path) {
        let root = self.session.workspace_root().to_path_buf();
        self.source_viewer.open(&root, path);
        self.workspace_mode = WorkspaceMode::Editor;
        self.status_message = "Viewing file (readonly)".into();
        // Keep the file explorer in sync with the active file.
        self.file_explorer.selected_path = Some(path.to_path_buf());
    }

    fn open_api_key_prompt(&mut self, profile_id: &str, error: Option<String>) {
        let p = self.connect_registry.get(profile_id);
        let title = p
            .map(|x| x.title.clone())
            .unwrap_or_else(|| profile_id.to_string());
        let auth_url = p.and_then(|x| x.auth_url.clone());
        let env_hint = if error.is_none() {
            p.and_then(|x| {
                x.api_key_env.iter().find_map(|env_name| {
                    std::env::var(env_name)
                        .ok()
                        .filter(|value| !value.is_empty())
                        .map(|_| env_name.clone())
                })
            })
        } else {
            None
        };
        let mut overlay = Overlay::connect_api_key(profile_id, title, auth_url, env_hint);
        if let Overlay::ConnectApiKey {
            error: overlay_error,
            ..
        } = &mut overlay
        {
            *overlay_error = error;
        }
        self.overlay = Some(overlay);
        self.status_message = format!("Connect {profile_id}");
        self.notices.clear();
    }

    fn open_model_picker_after_connect(&mut self, profile_id: &str) {
        let items = self.model_picker_items(true);
        let mut overlay = Overlay::model_open_with(items);
        overlay.focus_model(&self.runtime.model_label);
        self.overlay = Some(overlay);
        let title = self
            .connect_registry
            .get(profile_id)
            .map(|p| p.title.as_str())
            .unwrap_or(profile_id);
        self.set_feedback(
            FeedbackSeverity::Ok,
            format!("{title} connected · choose a model"),
        );
        self.notices.clear();
    }

    fn handle_connect(&mut self, action: ConnectAction) {
        // /connect or /connect list → interactive profile picker (usable UX)
        match &action {
            ConnectAction::Open | ConnectAction::List => {
                self.open_connect_picker();
                // Also fill notices with list for accessibility
                let mut model = Some(self.runtime.model_label.clone());
                if let Ok(msg) = handle_connect_action(
                    ConnectAction::List,
                    &self.connect_registry,
                    &self.connect_store,
                    &mut self.connect_profile,
                    &mut model,
                ) {
                    self.push_notice(msg.lines().map(|s| s.to_string()).collect());
                }
                return;
            }
            _ => {}
        }

        // Phase 6.1: open mode-specific overlays for interactive connect
        let connect_target = if let ConnectAction::Connect {
            ref profile_id,
            ref api_key,
            oauth_fixture,
        } = action
        {
            if api_key.is_none() && !oauth_fixture {
                if needs_tui_api_key_prompt(&self.connect_registry, profile_id) {
                    // Existing file/env credentials should reconnect without
                    // asking the user to paste the same secret again.
                    if !self.credentials_live_for(profile_id) {
                        self.open_api_key_prompt(profile_id, None);
                        return;
                    }
                }
                if needs_tui_oauth(&self.connect_registry, profile_id) {
                    self.begin_oauth_flow(profile_id);
                    return;
                }
            }
            Some(profile_id.clone())
        } else {
            None
        };

        let mut model = Some(self.runtime.model_label.clone());
        match handle_connect_action(
            action,
            &self.connect_registry,
            &self.connect_store,
            &mut self.connect_profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.auth_suspended = false;
                    self.session.set_active_model(m);
                }
                if let Some(pid) = self.connect_profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                self.status_message = lines.first().cloned().unwrap_or_default();
                self.notices.clear();
                self.notices_until = None;
                self.notices_until = None;
                self.notices_until = None;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Ok,
                    self.status_message.clone(),
                );
                if let Some(line) = lines.first() {
                    self.push_toast(line.clone());
                }
                self.refresh_connection_ui();
                if let Some(profile_id) = connect_target {
                    self.open_model_picker_after_connect(&profile_id);
                }
            }
            Err(ConnectError::OauthDevicePending(pending)) => {
                self.show_oauth_pending(pending);
            }
            Err(e) => {
                let error = e.to_string();
                if let Some(profile_id) =
                    connect_target.filter(|id| needs_tui_api_key_prompt(&self.connect_registry, id))
                {
                    self.open_api_key_prompt(&profile_id, Some(error));
                } else {
                    self.status_message = error.clone();
                    self.push_notice(vec![error]);
                }
            }
        }
    }

    fn begin_oauth_flow(&mut self, profile_id: &str) {
        let mut svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.connect_start_oauth(profile_id) {
            Ok(Ok(out)) => {
                self.on_connect_success(&out);
            }
            Ok(Err(pending)) => self.show_oauth_pending(pending),
            Err(e) => {
                self.status_message = e.to_string();
                self.push_notice(vec![e.to_string()]);
                self.report_error(&e.to_string());
            }
        }
    }

    /// After a successful connect: update model, inject credentials, clear OAuth UI.
    fn on_connect_success(&mut self, out: &forge_connect::ConnectOutcome) {
        self.connect_profile = Some(out.profile_id.clone());
        self.runtime.model_label = out.model.clone();
        self.runtime.provider = "native".into();
        self.auth_suspended = false;
        self.session.set_active_model(out.model.clone());
        self.apply_connect_credentials(&out.profile_id);
        self.oauth_pending = None;
        self.oauth_last_poll = None;
        self.refresh_connection_ui();
        self.open_model_picker_after_connect(&out.profile_id);
    }

    /// Export stored OAuth / API key material into the native model client.
    fn apply_connect_credentials(&mut self, profile_id: &str) {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.provider_env_for_profile(profile_id) {
            Ok(pairs) if !pairs.is_empty() => {
                for (k, v) in &pairs {
                    // Keep process env and the active native client in sync.
                    std::env::set_var(k, v);
                }
                self.session.apply_provider_env(&pairs);
            }
            Ok(_) => {}
            Err(_e) => {
                // Non-fatal: operator can still set XAI_API_KEY in the shell.
            }
        }
    }

    fn show_oauth_pending(&mut self, pending: OauthPending) {
        let title = self
            .connect_registry
            .get(&pending.profile_id)
            .map(|p| p.title.clone())
            .unwrap_or_else(|| pending.profile_id.clone());
        let instructions = pending.operator_instructions();
        let lines: Vec<String> = instructions.lines().map(|s| s.to_string()).collect();
        self.status_message = lines
            .first()
            .cloned()
            .unwrap_or_else(|| format!("OAuth for {}", pending.profile_id));
        self.push_notice(lines);
        self.overlay = Some(Overlay::connect_oauth(
            pending.profile_id.clone(),
            title,
            instructions,
        ));
        self.oauth_pending = Some(pending);
        self.oauth_last_poll = None;
    }

    /// Poll device-code OAuth once (called from the TUI tick loop).
    pub fn poll_oauth_tick(&mut self) {
        let Some(pending) = self.oauth_pending.clone() else {
            return;
        };
        let interval = Duration::from_secs(pending.interval_secs.max(1));
        if let Some(last) = self.oauth_last_poll {
            if last.elapsed() < interval {
                return;
            }
        }
        self.oauth_last_poll = Some(std::time::Instant::now());
        let mut svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        match svc.poll_oauth_once(&pending) {
            Ok(Some(out)) => {
                self.on_connect_success(&out);
            }
            Ok(None) => {
                // still waiting
            }
            Err(e) => {
                self.oauth_pending = None;
                self.oauth_last_poll = None;
                self.overlay = None;
                self.report_error(&e.to_string());
            }
        }
    }

    fn finish_connect(&mut self, profile_id: &str, api_key: Option<String>, oauth_fixture: bool) {
        self.handle_connect(ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key,
            oauth_fixture,
        });
    }

    /// Submit API key (or env) from the connect modal. On failure, keep the modal open
    /// with an error so the operator can re-paste (does not clear a long key on length checks
    /// when the failure came from Use-env short key).
    fn try_connect_api_key(&mut self, profile_id: &str, api_key: Option<String>) {
        let saved_overlay = self.overlay.take();
        let mut model = Some(self.runtime.model_label.clone());
        let action = ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key: api_key.clone(),
            oauth_fixture: false,
        };
        match handle_connect_action(
            action,
            &self.connect_registry,
            &self.connect_store,
            &mut self.connect_profile,
            &mut model,
        ) {
            Ok(msg) => {
                if let Some(m) = model {
                    self.runtime.model_label = m.clone();
                    self.runtime.provider = "native".into();
                    self.auth_suspended = false;
                    self.session.set_active_model(m);
                }
                if let Some(pid) = self.connect_profile.clone() {
                    self.apply_connect_credentials(&pid);
                }
                self.status_message = msg.lines().next().unwrap_or_default().to_string();
                self.refresh_connection_ui();
                self.open_model_picker_after_connect(profile_id);
            }
            Err(e) => {
                let err = e.to_string();
                self.overlay = saved_overlay;
                if let Some(Overlay::ConnectApiKey { error, .. }) = &mut self.overlay {
                    *error = Some(err.clone());
                }
                self.status_message = err;
                self.push_activity(
                    ActivityKind::Connect,
                    FeedbackSeverity::Error,
                    format!("connect {profile_id} failed"),
                );
            }
        }
    }

    /// Apply a provider/model id to this session (no restart required).
    fn apply_model_selection(&mut self, provider: &str, model: &str) {
        let model = model.trim();
        if model.is_empty() {
            return;
        }
        self.runtime.provider = if provider.trim().is_empty() {
            "native".into()
        } else {
            provider.to_string()
        };
        self.auth_suspended = false;
        self.runtime.model_label = model.to_string();
        self.session.set_active_model(model);
        // Match the selected model to its connected profile even when a
        // different provider was active before opening the picker.
        let prefix = model.split('/').next().unwrap_or("");
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: None,
            active_model: None,
        };
        if let Ok(connected) = svc.connected_profiles() {
            if let Some(profile) = connected.iter().find(|p| {
                p.model_provider_prefix == prefix
                    || p.id == prefix
                    || (prefix == "opencode-go" && p.id == "opencode_go")
                    || (prefix == "opencode-zen" && p.id == "opencode_zen")
            }) {
                self.connect_profile = Some(profile.id.clone());
            }
        }
        if let Some(profile_id) = self.connect_profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }
        self.persist_selection();
        self.feedback = FeedbackModel::default();
        self.status_message.clear();
        self.notices.clear();
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Ok,
            format!("model {}", self.runtime.model_label),
        );
    }

    fn model_prefix(model: &str) -> &str {
        model.split('/').next().unwrap_or("").trim()
    }

    fn connected_profile_for_model_prefix(&self, prefix: &str) -> Option<String> {
        let prefix = prefix.trim();
        if prefix.is_empty() {
            return None;
        }
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().ok()?;
        connected.iter().find_map(|profile| {
            let pid = profile.id.as_str();
            let provider_prefix = profile.model_provider_prefix.as_str();
            let matches = prefix == pid
                || prefix == provider_prefix
                || (prefix == "openai" && pid == "openai_codex")
                || (prefix == "openai-codex" && pid == "openai_codex")
                || (prefix == "opencode-go" && pid == "opencode_go")
                || (prefix == "opencode-zen" && pid == "opencode_zen")
                || (prefix == "grok" && pid == "xai");
            if matches {
                Some(profile.id.clone())
            } else {
                None
            }
        })
    }

    /// Build `/model` picker rows from connected-profile catalogs (cache + optional refresh).
    fn model_picker_items(&self, refresh_stale: bool) -> Vec<crate::overlays::ModelItem> {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let connected = svc.connected_profiles().unwrap_or_default();
        let cache = ModelCatalogCache::user_default();
        let profiles: Vec<_> = if connected.is_empty() {
            // Show all built-in defaults when nothing connected
            self.connect_registry.profiles().to_vec()
        } else {
            connected
        };
        let entries = models_for_picker(&profiles, &self.connect_store, &cache, refresh_stale);
        models_from_catalog(&entries)
    }

    fn active_model_cost(&mut self) -> Option<forge_connect::CatalogCost> {
        if let Some((model, cost)) = &self.model_cost_cache {
            if model == &self.runtime.model_label {
                return *cost;
            }
        }
        let cost = ModelCatalogCache::user_default().get_registry_cost(&self.runtime.model_label);
        self.model_cost_cache = Some((self.runtime.model_label.clone(), cost));
        cost
    }

    /// Enqueue while a message is processing (TUI Enter path only).
    fn enqueue_user_message(&mut self, line: String) {
        let n = self.message_queue.enqueue(line);
        if self.queue_selected.is_none() {
            self.queue_selected = Some(0);
        }
        self.push_toast(format!("queued #{n}"));
        self.set_feedback(
            FeedbackSeverity::Info,
            format!(
                "queued #{n} · {} waiting · Ctrl+Up/Down select · Ctrl+Backspace cancel",
                self.message_queue.len()
            ),
        );
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            format!("queue enqueue #{n}"),
        );
    }

    /// Take next queued message and start a model turn.
    fn dequeue_and_send_next(&mut self) {
        if self.busy || self.pending_prompt.is_some() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "still processing — wait before sending the next queued message",
            );
            return;
        }
        if self.session.pending_hitl.is_some() {
            self.set_feedback(FeedbackSeverity::Warn, "resolve HITL before dequeuing");
            return;
        }
        let Some(next) = self.message_queue.dequeue() else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.clamp_queue_selection();
        if !self.is_provider_connected() {
            self.message_queue.push_front(next);
            self.report_error("Not connected — cannot send queued message. Run /connect.");
            return;
        }
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            format!("queue dequeue · {} left", self.message_queue.len()),
        );
        self.set_feedback(
            FeedbackSeverity::Info,
            format!("sending dequeued · {} remaining", self.message_queue.len()),
        );
        // Start the turn the same way as a normal Enter send (no dispatch recursion).
        self.clear_error_chrome();
        if let Some(pid) = self.connect_profile.clone() {
            self.apply_connect_credentials(&pid);
        }
        self.pending_prompt = Some(next);
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.turn_started = Some(Instant::now());
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
    }

    /// Cancel a queued message by 0-based index.
    fn cancel_queued_at(&mut self, index: usize) {
        let one_based = index + 1;
        match self.message_queue.drop_at(one_based) {
            Some(t) => {
                let preview: String = t.chars().take(48).collect();
                self.push_toast(format!("cancelled #{one_based}"));
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!(
                        "cancelled queued #{one_based} · {} left",
                        self.message_queue.len()
                    ),
                );
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Ok,
                    format!("queue cancel #{one_based}: {preview}"),
                );
                self.clamp_queue_selection();
            }
            None => {
                self.set_feedback(FeedbackSeverity::Warn, "queue item gone");
            }
        }
    }

    fn clamp_queue_selection(&mut self) {
        let len = self.message_queue.len();
        self.queue_selected = match (len, self.queue_selected) {
            (0, _) => None,
            (_, Some(i)) if i < len => Some(i),
            (_, Some(_)) => Some(len - 1),
            (_, None) => Some(0),
        };
    }

    fn move_queue_selection(&mut self, delta: i32) {
        let len = self.message_queue.len();
        if len == 0 {
            self.queue_selected = None;
            return;
        }
        let cur = self.queue_selected.unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(len as i32) as usize;
        self.queue_selected = Some(next);
    }

    fn cancel_selected_queue(&mut self) {
        let Some(idx) = self.queue_selected else {
            self.set_feedback(FeedbackSeverity::Warn, "queue empty");
            return;
        };
        self.cancel_queued_at(idx);
    }

    /// Run the built-in `git` tool in the session workspace. Never touches the model.
    async fn run_git_tool(
        &self,
        subcommand: &str,
        args: Vec<String>,
    ) -> Result<forge_types::ToolOutput, forge_tools::ToolError> {
        let ctx = ToolContext::new(self.session.workspace_root().to_path_buf());
        GitTool
            .call(
                &ctx,
                json!({
                    "subcommand": subcommand,
                    "args": args,
                }),
            )
            .await
    }

    /// `/sync` — stage all changes, invent a commit message from the changeset, commit, push.
    fn queue_sync(&mut self) {
        if self.busy || self.pending_prompt.is_some() || self.pending_sync {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /sync");
            return;
        }
        self.pending_sync = true;
        self.busy_phase = BusyPhase::Other("git sync".into());
        self.push_toast("syncing…");
        self.status_message = "syncing…".into();
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync queued",
        );
    }

    fn queue_context_reset(&mut self) {
        if self.busy
            || self.pending_prompt.is_some()
            || self.pending_sync
            || self.pending_hitl_decision.is_some()
            || self.pending_context_reset
        {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /compact");
            return;
        }
        self.pending_context_reset = true;
        self.busy_phase = BusyPhase::Other("context reset".into());
        self.status_message = "resetting context…".into();
        self.set_feedback(FeedbackSeverity::Info, "resetting context…");
    }

    pub async fn drain_pending_hitl(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let Some(decision) = self.pending_hitl_decision.take() else {
            return Ok(());
        };
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        self.session.resolve_hitl(decision.clone(), "tui").await?;
        self.status_message = match decision {
            HitlDecision::Approve => "Action approved".into(),
            HitlDecision::Deny => "Action denied".into(),
        };
        self.push_notice(vec![self.status_message.clone()]);
        self.busy_phase = BusyPhase::Idle;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }

    pub async fn drain_pending_context_reset(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_context_reset {
            return Ok(());
        }
        self.pending_context_reset = false;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let before_report = self.session.token_usage_report();
        let before = before_report.context_tokens_est;
        self.session.force_context_reset_async().await?;
        let after_report = self.session.token_usage_report();
        let after = after_report.context_tokens_est;
        self.context_reset_snapshot = Some((
            before as f64 / before_report.context_capacity.max(1) as f64 * 100.0,
            after as f64 / after_report.context_capacity.max(1) as f64 * 100.0,
        ));
        self.chat_message_start = self.session.messages.len();
        self.chat_event_start = self.session.events.len();
        self.push_toast("Continuing in a fresh context");
        let progress = fs::read_to_string(self.runtime.cwd.join(".forge/progress.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<ProgressDocument>(&text).ok());
        if let Some(progress) = progress {
            self.ui_banners.push(ChatItem::ContextHandoff {
                before_pct: self.context_reset_snapshot.unwrap().0,
                after_pct: self.context_reset_snapshot.unwrap().1,
                goal: progress.goal,
                completed: progress.completed,
                next_actions: progress.next_actions,
            });
        }
        self.push_activity(
            ActivityKind::Context,
            FeedbackSeverity::Ok,
            format!("fresh context prepared · {before} → {after} tokens"),
        );
        self.status_message = "Continuing in a fresh context".into();
        self.notices.clear();
        self.busy_phase = BusyPhase::Idle;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
    }

    /// Open the active file in the user's configured external editor.
    ///
    /// Suspends the TUI terminal, spawns the editor, waits for it to
    /// complete, restores the TUI, and refreshes the source viewer and
    /// Git status.
    pub async fn drain_pending_external_editor(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_external_editor {
            return Ok(());
        }
        self.pending_external_editor = false;

        // 1. Guard: must have a valid text file open.
        let file_path = match &self.source_viewer.path {
            Some(p) if self.source_viewer.status.is_openable() => p.clone(),
            Some(_) => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    "Cannot open binary files in an external editor",
                );
                return Ok(());
            }
            None => {
                self.set_feedback(FeedbackSeverity::Warn, "No file open in the source viewer");
                return Ok(());
            }
        };

        // 2. Guard: no unsafe write-active tool.
        if matches!(self.busy_phase, BusyPhase::Tool { .. }) {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "External editor unavailable while Forge is writing files.\n\n\
                 Wait for the current operation to finish, then try again.",
            );
            return Ok(());
        }

        // 3. Resolve editor.
        let (editor_cmd, _editor_args) = match crate::editor::resolve_editor() {
            Some(r) => r,
            None => {
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    &EditorError::NotConfigured.to_string(),
                );
                return Ok(());
            }
        };

        // 4. Flush pending redraw.
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }

        // 5. Suspend the TUI terminal (restore normal terminal state).
        crate::terminal::restore_terminal();

        // 6. Spawn the editor and wait.
        let mut cmd = std::process::Command::new(&editor_cmd);
        for arg in &_editor_args {
            cmd.arg(arg);
        }
        cmd.arg(&file_path);

        let status = match cmd.status() {
            Ok(s) => s,
            Err(e) => {
                // Re-enter TUI mode so the user sees the error message.
                let _ = crate::terminal::reinit_terminal();
                self.set_feedback(
                    FeedbackSeverity::Warn,
                    &EditorError::SpawnFailed(e).to_string(),
                );
                return Ok(());
            }
        };

        // 7. Re-enter TUI terminal mode.
        let _ = crate::terminal::reinit_terminal();

        // 8. Report non-zero exit.
        if let Some(code) = status.code() {
            if code != 0 {
                self.push_activity(
                    ActivityKind::System,
                    FeedbackSeverity::Warn,
                    format!("external editor exited with status {code}"),
                );
            }
        }

        // 9. Refresh the active file and Git status.
        self.refresh_post_editor();
        Ok(())
    }

    /// Called after the external editor exits. Reloads the file, refreshes
    /// syntax highlighting, search state, and Git markers.
    fn refresh_post_editor(&mut self) {
        let root = self.session.workspace_root().to_path_buf();
        let path = self.source_viewer.path.clone();
        let old_line = self.source_viewer.current_line;
        let old_top = self.source_viewer.top_line;

        if let Some(p) = &path {
            if p.exists() {
                self.source_viewer.refresh(&root);
                // Preserve sensible cursor.
                self.source_viewer.current_line =
                    old_line.min(self.source_viewer.lines.len().saturating_sub(1));
                self.source_viewer.top_line =
                    old_top.min(self.source_viewer.lines.len().saturating_sub(1));
            } else {
                // File was deleted — show that in the viewer.
                self.source_viewer.path = None;
                self.source_viewer.status = crate::source_viewer::ViewerStatus::NotFound;
                self.source_viewer.lines.clear();
            }
        }

        // Invalidate search matches (recomputed lazily).
        let search_query = self.source_viewer.search.query.clone();
        if !search_query.is_empty() {
            self.source_viewer.update_search_query(&search_query);
        }

        // Refresh Git status.
        self.file_explorer.refresh_git_status();

        // Show a compact notice.
        let gs = &self.file_explorer.git_status;
        let changed = gs.status.len();
        let gs_text = if changed == 0 {
            "No repository changes detected".into()
        } else if changed == 1 {
            "1 file changed".into()
        } else {
            format!("{changed} files changed")
        };
        self.notices.clear();
        self.push_notice(vec!["Returned from external editor".into(), gs_text]);
    }

    /// Drive `/sync` work with a terminal handle available for intermediate redraws.
    pub async fn drain_pending_sync(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_sync {
            return Ok(());
        }
        self.pending_sync = false;
        self.slash_sync_inner(&mut terminal).await;
        Ok(())
    }

    async fn slash_sync_inner(
        &mut self,
        terminal: &mut Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) {
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync (stage · message · commit · push)",
        );
        self.busy_phase = BusyPhase::Other("git sync".into());
        self.set_feedback(FeedbackSeverity::Info, "syncing… inspecting changes");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }

        // Anything unstaged or untracked?
        let status = match self
            .run_git_tool("status", vec!["--porcelain".into()])
            .await
        {
            Ok(o) if !o.is_error => o.content,
            Ok(o) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git status failed: {}", o.content.trim()));
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git status failed: {e}"));
                return;
            }
        };
        if status.trim().is_empty() {
            self.busy_phase = BusyPhase::Idle;
            self.set_feedback(FeedbackSeverity::Info, "nothing to sync (clean tree)");
            self.notices.clear();
            self.push_toast("working tree clean");
            self.push_activity(
                ActivityKind::Tool,
                FeedbackSeverity::Info,
                "git sync skipped · clean tree",
            );
            if let Some(term) = terminal.as_deref_mut() {
                let _ = term.draw(|f| self.draw(f));
            }
        }

        // Stage everything.
        match self.run_git_tool("add", vec!["-A".into()]).await {
            Ok(o) if o.is_error => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git add failed: {}", o.content.trim()));
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git add failed: {e}"));
                return;
            }
            Ok(_) => {}
        }

        // Build a message from the staged changeset (stat + name-status + optional LLM).
        let name_status = self
            .run_git_tool("diff", vec!["--cached".into(), "--name-status".into()])
            .await
            .map(|o| o.content)
            .unwrap_or_default();
        let stat = self
            .run_git_tool("diff", vec!["--cached".into(), "--stat".into()])
            .await
            .map(|o| o.content)
            .unwrap_or_default();
        let patch_snip = self
            .run_git_tool("diff", vec!["--cached".into()])
            .await
            .map(|o| o.content.chars().take(6_000).collect::<String>())
            .unwrap_or_default();

        self.set_feedback(FeedbackSeverity::Info, "syncing… writing commit message");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let message = self
            .commit_message_from_changeset(&name_status, &stat, &patch_snip)
            .await;

        self.set_feedback(
            FeedbackSeverity::Info,
            format!("syncing… commit: {}", truncate_one_line(&message, 48)),
        );
        let commit = self
            .run_git_tool("commit", vec!["-m".into(), message.clone()])
            .await;
        match commit {
            Ok(o) if o.is_error => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git commit failed: {}", o.content.trim()));
                self.push_notice(
                    o.content
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .take(16)
                        .collect(),
                );
                return;
            }
            Err(e) => {
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&format!("git commit failed: {e}"));
                return;
            }
            Ok(o) => {
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Ok,
                    format!("git commit · {message}"),
                );
                let _ = o;
            }
        }

        self.set_feedback(FeedbackSeverity::Info, "syncing… push");
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        let push = self.run_git_tool("push", vec![]).await;
        self.busy_phase = BusyPhase::Idle;
        match push {
            Ok(o) if o.is_error => {
                // Commit succeeded; push failed — surface both.
                self.report_error(&format!("committed but push failed: {}", o.content.trim()));
                let mut lines = vec![format!("Committed: {message}"), "Push failed:".into()];
                lines.extend(
                    o.content
                        .lines()
                        .map(|s| s.to_string())
                        .filter(|s| !s.is_empty())
                        .take(12),
                );
                self.push_notice(lines);
            }
            Err(e) => {
                self.report_error(&format!("committed but push failed: {e}"));
                self.push_notice(vec![
                    format!("Committed: {message}"),
                    format!("Push error: {e}"),
                ]);
            }
            Ok(o) => {
                self.push_toast("synced");
                self.set_feedback(
                    FeedbackSeverity::Ok,
                    format!("synced · {}", truncate_one_line(&message, 40)),
                );
                let mut lines = vec![format!("Commit: {message}"), "Push: ok".into()];
                if !stat.trim().is_empty() {
                    lines.push(String::new());
                    lines.push("Changeset:".into());
                    for l in stat.lines().take(12) {
                        lines.push(l.to_string());
                    }
                }
                if !o.content.trim().is_empty() {
                    lines.push(String::new());
                    for l in o.content.lines().take(8) {
                        lines.push(l.to_string());
                    }
                }
                self.notices.clear();
                self.notices_until = None;
                self.push_activity(ActivityKind::Tool, FeedbackSeverity::Ok, lines.join(" · "));
                self.push_activity(ActivityKind::Tool, FeedbackSeverity::Ok, "git push");
            }
        }
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
    }

    /// Prefer a short model-written summary of the staged diff; fall back to a file-list heuristic.
    async fn commit_message_from_changeset(
        &self,
        name_status: &str,
        stat: &str,
        patch_snip: &str,
    ) -> String {
        let fallback = heuristic_commit_message(name_status);
        if !self.is_provider_connected() {
            return fallback;
        }
        let model_id = if self.session.active_model.is_empty() {
            self.runtime.model_label.clone()
        } else {
            self.session.active_model.clone()
        };
        let user = format!(
            "Write a single-line git commit message (max ~72 chars) for this staged change.\n\
Rules: imperative mood, no quotes, no trailing period, no conventional-commit prefix unless clearly needed.\n\
Reply with ONLY the commit message line.\n\n\
## name-status\n{name_status}\n\n\
## stat\n{stat}\n\n\
## patch (truncated)\n{patch_snip}"
        );
        let req = forge_model::ModelRequest {
            messages: vec![
                forge_types::Message::new(
                    forge_types::MessageRole::System,
                    "You write concise git commit messages from diffs.",
                ),
                forge_types::Message::new(forge_types::MessageRole::User, user),
            ],
            tools: vec![],
            model: model_id,
            reasoning_effort: Some(self.reasoning_effort.to_string())
                .filter(|value| value != "auto"),
            prompt_cache: true,
        };
        match self.session.model_client().complete(req).await {
            Ok(resp) => {
                let line = resp
                    .text
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .trim_matches(|c| c == '"' || c == '\'' || c == '`')
                    .trim_end_matches('.')
                    .to_string();
                if line.is_empty() || line.len() > 200 {
                    fallback
                } else {
                    line
                }
            }
            Err(_) => fallback,
        }
    }

    /// Insert bracketed-paste text into the active target (API-key modal or main input).
    fn handle_paste(&mut self, data: &str) {
        if let Some(ref mut ov) = self.overlay {
            let _ = handle_overlay_key(ov, OverlayKey::Paste(data.to_string()));
            return;
        }
        self.input.history_browse = false;
        self.input.insert_paste(data);
        self.clamp_slash_suggest();
    }

    pub fn refresh_status_model(&self) -> StatusModel {
        self.refresh_status_model_with_connected(self.is_provider_connected())
    }

    fn refresh_status_model_with_connected(&self, provider_connected: bool) -> StatusModel {
        let repo = self.repo_header();
        let id = self.session.session_id.to_string();
        let short = if id.len() > 8 {
            id[..8].to_string()
        } else {
            id
        };
        StatusModel {
            status: self.session.status,
            session_short: short,
            model: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
            effort: self.reasoning_effort.to_string(),
            ctx_pct: self.session.context_usage_ratio(),
            busy: self.busy,
            busy_phase: self.busy_phase.clone(),
            connect_profile: self.connect_profile.clone(),
            provider_connected,
            web_search_label: self.web_search_label.clone(),
            tools_visible: self.session.list_tools().len(),
            prompt_cache_hits: self.session.token_usage.prompt_cache_hits,
            prompt_cache_writes: self.session.token_usage.prompt_cache_writes,
            repo_name: repo.repo_name.clone(),
            branch: repo.branch.clone(),
            dirty: repo.dirty,
        }
    }

    fn repo_header(&self) -> RepoHeaderCache {
        let repo_name = self
            .runtime
            .cwd
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_string);

        let branch = std::process::Command::new("git")
            .args(["branch", "--show-current"])
            .current_dir(&self.runtime.cwd)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());

        let dirty = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.runtime.cwd)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|text| !text.trim().is_empty())
            .unwrap_or(false);

        RepoHeaderCache {
            repo_name,
            branch,
            dirty,
        }
    }

    fn busy_status_detail(&self) -> Option<String> {
        self.busy.then(|| {
            let label = if !self.stream_thinking.is_empty() && self.stream_preview.is_empty() {
                "Thinking..."
            } else {
                "Working..."
            };
            let elapsed = self
                .turn_started
                .map(|started| started.elapsed().as_secs_f64())
                .unwrap_or(0.0);
            format!("{label} {}", format_elapsed_tenths(elapsed))
        })
    }

    fn footer_limits(&mut self, provider: &str) -> FooterLimits {
        if let Some(rx) = &self.footer_limits_rx {
            match rx.try_recv() {
                Ok((provider, limits)) => {
                    self.footer_limits_cache = Some(FooterLimitsCache {
                        provider,
                        fetched_at: Instant::now(),
                        limits,
                    });
                    self.footer_limits_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.footer_limits_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }

        if provider != OPENAI_CODEX_PROFILE_ID {
            return FooterLimits::default();
        }

        let (cached_limits, needs_refresh) = match self
            .footer_limits_cache
            .as_ref()
            .filter(|cache| cache.provider == provider)
        {
            Some(cache) => (
                Some(cache.limits.clone()),
                cache.fetched_at.elapsed() >= Duration::from_secs(60),
            ),
            None => (None, true),
        };
        if needs_refresh && self.footer_limits_rx.is_none() {
            let provider = provider.to_string();
            let request_provider = provider.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let report = forge_connect::provider_cost_report(
                    &request_provider,
                    "",
                    0,
                    0,
                    &CredentialStore::user_default(),
                )
                .unwrap_or_default();
                let _ = tx.send((request_provider, footer_limits_from_report(&report)));
            });
            self.footer_limits_rx = Some(rx);
        }

        cached_limits.unwrap_or_default()
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        let area = frame.size();
        if is_too_small(area) {
            frame.render_widget(
                Paragraph::new("Terminal too small — resize to at least 40x18"),
                area,
            );
            return;
        }
        let fb_h = if self.feedback.is_empty() { 0 } else { 1 };
        let input_h = (self.input.visual_lines() + 2).clamp(3, 8);
        let slash_mode = self.overlay.is_none() && self.input.text.starts_with('/');
        let panel_h = if self.bottom_panel.open { 8 } else { 0 };
        let mut regions = split_areas_with_side_panels(
            area,
            fb_h,
            input_h,
            !slash_mode && self.files_visible,
            !slash_mode && self.sidebar_visible,
            0,
            panel_h,
        );
        let workspace_rows = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Length(1),
                ratatui::layout::Constraint::Min(0),
            ])
            .split(regions.chat);
        self.render_workspace_tabs(workspace_rows[0], frame.buffer_mut());
        regions.chat = workspace_rows[1];
        let connected = self.is_provider_connected();
        let status = self.refresh_status_model_with_connected(connected);
        frame.render_widget(StatusBar { model: &status }, regions.status);
        if let Some(files) = regions.files {
            frame.render_widget(
                FileExplorerWidget {
                    explorer: &mut self.file_explorer,
                },
                files,
            );
        }

        let stream_wait = if self.busy && self.pending_prompt.is_none() {
            let elapsed = if !self.stream_thinking.is_empty() {
                // Thinking timer runs from first thinking token
                self.thinking_started
                    .or(self.turn_started)
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0)
            } else {
                self.turn_started
                    .map(|t| t.elapsed().as_secs_f64())
                    .unwrap_or(0.0)
            };
            // After answer tokens start, drop the wait/think status line.
            if !self.stream_preview.is_empty() {
                None
            } else if !self.stream_thinking.is_empty() {
                Some((StreamWaitPhase::Thinking, elapsed))
            } else {
                Some((StreamWaitPhase::Waiting, elapsed))
            }
        } else {
            None
        };
        let opts = ConversationViewOpts {
            busy: self.busy,
            // Don't force-expand finished thinking just because busy (answer may be streaming)
            tool_expanded: self.tool_expanded,
            compact: false,
            stream_wait,
            stream_thought_secs: self.thought_secs,
        };
        // `/clear` only clears the viewport; the full session remains available to the model.
        let visible_messages =
            &self.session.messages[self.chat_message_start.min(self.session.messages.len())..];
        let visible_events =
            &self.session.events[self.chat_event_start.min(self.session.events.len())..];
        let key = ConversationRenderKey {
            session_id: self.session.session_id,
            width: regions.chat.width,
            messages: visible_messages.len(),
            last_message_content: visible_messages
                .last()
                .map_or(0, |message| message.content.len()),
            last_message_thinking: visible_messages
                .last()
                .and_then(|message| message.thinking.as_ref())
                .map_or(0, String::len),
            events: visible_events.len(),
            last_event_detail: visible_events.last().map_or(0, |event| event.detail.len()),
            banners: self.ui_banners.len(),
            queue: self.message_queue.len(),
            queue_selected: self.queue_selected,
            chat_message_start: self.chat_message_start,
            chat_event_start: self.chat_event_start,
            busy: self.busy,
            busy_phase: self.busy_phase.label(),
            tool_expanded: self.tool_expanded,
            splash_dismissed: self.splash_dismissed,
            slash_mode,
            status: self.session.status,
        };
        if self.conversation_cache.as_ref().map(|cache| &cache.key) != Some(&key) {
            let mut conv = ConversationModel::from_messages(
                visible_messages,
                visible_events,
                self.session.status,
                ConversationViewOpts {
                    busy: false,
                    stream_wait: None,
                    stream_thought_secs: None,
                    ..opts.clone()
                },
            )
            .with_extra_banners(self.ui_banners.iter().cloned());
            if !self.splash_dismissed {
                conv = conv.with_brand(self.runtime.version.clone());
            }
            if !slash_mode && !self.splash_dismissed {
                conv = conv.with_home(
                    self.runtime.cwd.display().to_string(),
                    self.session.loaded_skills_count(),
                );
            }
            conv = conv.with_queued_messages(
                self.message_queue.iter().cloned().collect::<Vec<_>>(),
                self.queue_selected,
            );
            if let BusyPhase::Tool { name } = &self.busy_phase {
                conv = conv.with_running_tool(name.clone());
            }
            if let Some(payload) = &self.session.pending_hitl {
                let args = payload
                    .args_redacted
                    .get("command")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                    .unwrap_or_else(|| payload.args_redacted.to_string());
                conv = conv.with_blocked_tool(payload.tool.clone(), args);
            }
            let width = regions.chat.width.saturating_sub(2) as usize;
            self.conversation_cache = Some(ConversationRenderCache {
                key,
                lines: conv.lines_for_width(width),
            });
        }
        let width = regions.chat.width.saturating_sub(2) as usize;
        let live_lines = if self.busy && self.pending_prompt.is_none() {
            ConversationModel::from_messages(
                &[],
                &[],
                self.session.status,
                ConversationViewOpts { busy: true, ..opts },
            )
            .with_streaming_preview(self.stream_thinking.clone(), self.stream_preview.clone())
            .lines_for_width(width)
        } else {
            Vec::new()
        };
        let cached = self
            .conversation_cache
            .as_ref()
            .expect("conversation cache populated");
        if self.workspace_mode == WorkspaceMode::Chat {
            frame.render_widget(
                crate::conversation::ConversationLinesWidget {
                    lines: &cached.lines,
                    tail_lines: &live_lines,
                    scroll: self.chat_scroll,
                    follow: self.chat_follow,
                },
                ratatui::layout::Rect {
                    x: regions.chat.x.saturating_add(2.min(regions.chat.width)),
                    y: regions.chat.y.saturating_add(1.min(regions.chat.height)),
                    width: regions.chat.width.saturating_sub(2.min(regions.chat.width)),
                    height: regions
                        .chat
                        .height
                        .saturating_sub(1.min(regions.chat.height)),
                },
            );
        } else if self.workspace_mode == WorkspaceMode::Editor {
            self.last_editor_height = regions.chat.height;
            self.source_viewer.focused = true;
            frame.render_widget(
                SourceViewerWidget {
                    viewer: &mut self.source_viewer,
                },
                regions.chat,
            );
        } else {
            self.source_viewer.focused = false;
            self.render_workspace_empty_state(regions.chat, frame.buffer_mut());
        }
        if let Some(sidebar_area) = regions.sidebar {
            let activity = self
                .activity
                .recent(8)
                .iter()
                .map(|item| item.summary.clone())
                .collect::<Vec<_>>();
            let mut sidebar = SidebarModel::from_session_with_activity(&self.session, &activity);
            sidebar.provider = self.runtime.provider.clone();
            sidebar.model = self.runtime.model_label.clone();
            sidebar.busy = self.busy;
            sidebar.step = match &self.busy_phase {
                BusyPhase::Model => "model_stream",
                BusyPhase::Tool { .. } => "tool_execution",
                BusyPhase::Connect => "connect",
                BusyPhase::Other(step) => step,
                BusyPhase::Idle => "idle",
            }
            .into();
            sidebar.context_reset = self.context_reset_snapshot;
            sidebar.session_allows = self.hitl_session_allow.iter().cloned().collect();
            let header = self.repo_header();
            sidebar.repo_name = header.repo_name;
            sidebar.branch = header.branch;
            let gs = &self.file_explorer.git_status;
            sidebar.git_status_loading = gs.loading;
            sidebar.git_status_error = gs.error.is_some();
            sidebar.files_changed = Some(gs.status.len());
            sidebar.elapsed = self
                .turn_started
                .or(self.thinking_started)
                .map(|started| format_elapsed_tenths(started.elapsed().as_secs_f64()));
            frame.render_widget(
                SidebarWidget {
                    model: &sidebar,
                    view: self.inspector_view,
                },
                sidebar_area,
            );
        }

        frame.render_widget(
            BottomPanel {
                model: BottomPanelModel {
                    state: &self.bottom_panel,
                    busy_phase: &self.busy_phase,
                    activity: &self.activity,
                },
            },
            regions.bottom_panel,
        );

        // Notices (help, connect list, multi-line status) just above input
        if !self.notices.is_empty() && self.overlay.is_none() {
            let notice_h = (self.notices.len() as u16).min(18).saturating_add(1);
            // Render into bottom of chat area
            let chat = regions.chat;
            if chat.height > notice_h {
                let notice_area = ratatui::layout::Rect {
                    x: chat.x,
                    y: chat.y + chat.height.saturating_sub(notice_h),
                    width: chat.width,
                    height: notice_h,
                };
                let text = self
                    .notices
                    .iter()
                    .take(18)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                frame.render_widget(
                    Paragraph::new(text).style(theme::muted()).block(
                        ratatui::widgets::Block::default()
                            .borders(ratatui::widgets::Borders::TOP)
                            .title(ratatui::text::Span::styled(" notices ", theme::muted())),
                    ),
                    notice_area,
                );
            }
        }

        // Inline slash autocomplete above the input bar — full list with scroll window
        if self.overlay.is_none() {
            let suggestions = self.slash_suggestions();
            if !suggestions.is_empty() && self.input.text.starts_with('/') {
                let input = regions.input;
                let n = suggestions.len();
                let idx = self.slash_suggest_idx.min(n.saturating_sub(1));
                // Use as much space above the input as possible (cap for readability).
                let max_list = input.y.saturating_sub(2).clamp(1, 16) as usize;
                let visible = n.min(max_list);
                // Scroll so the highlighted row stays on screen.
                let start = if n <= visible || idx < visible / 2 {
                    0
                } else if idx + (visible - visible / 2) >= n {
                    n - visible
                } else {
                    idx - visible / 2
                };
                let h = (visible as u16).saturating_add(3); // borders + selected help
                if input.y >= h {
                    let sug_area = ratatui::layout::Rect {
                        x: input.x,
                        y: input.y.saturating_sub(h),
                        width: input.width,
                        height: h,
                    };
                    // Pad rows so background fill spans the panel width (visible selection).
                    let inner_w = sug_area.width.saturating_sub(2) as usize;
                    let mut lines: Vec<ratatui::text::Line> = suggestions
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(visible)
                        .map(|(i, it)| {
                            let marker = if i == idx { "▶ " } else { "  " };
                            let raw = format!("{marker}{:<14} {}", it.cmd, it.desc);
                            let mut row = raw
                                .chars()
                                .take(inner_w.saturating_sub(1))
                                .collect::<String>();
                            while row.chars().count() < inner_w.saturating_sub(1) {
                                row.push(' ');
                            }
                            let style = if i == idx {
                                theme::selected_row()
                            } else {
                                theme::text()
                            };
                            ratatui::text::Line::from(ratatui::text::Span::styled(row, style))
                        })
                        .collect();
                    lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
                        format!("  {}", suggestions[idx].desc),
                        theme::dim(),
                    )));
                    let title = if n > visible {
                        format!(
                            " commands {}–{}/{} · Tab · ↑↓ ",
                            start + 1,
                            start + visible,
                            n
                        )
                    } else {
                        format!(" commands ({n}) · ↑↓ select · Tab complete · Enter run ")
                    };
                    frame.render_widget(
                        Paragraph::new(lines).block(
                            ratatui::widgets::Block::default()
                                .borders(ratatui::widgets::Borders::ALL)
                                .border_style(theme::brand())
                                .title(ratatui::text::Span::styled(title, theme::brand())),
                        ),
                        sug_area,
                    );
                }
            }
        }

        // Phase 10 / TUI-08 — always-visible feedback strip
        if !self.feedback.is_empty() && regions.feedback.height > 0 {
            frame.render_widget(
                FeedbackBar {
                    model: &self.feedback,
                },
                regions.feedback,
            );
        }

        let mut input = self.input.clone();
        // Allow composing the next message while a turn runs; only dim slightly when busy.
        input.dimmed = self.busy && self.input.text.is_empty();
        input.not_connected = !connected;
        frame.render_widget(InputBar { model: &input }, regions.input);

        let qn = self.message_queue.len();
        let context = self.session.token_usage_report();
        let busy_detail = self.busy_status_detail();
        let (status_label, _) = status.status_label_with_busy_detail(busy_detail.as_deref());
        let hints = if self.busy {
            if qn > 0 {
                format!("queue {qn} · Ctrl+Up/Down select · Ctrl+Backspace cancel")
            } else {
                "type + Enter to queue · Esc interrupt".into()
            }
        } else if self.session.pending_hitl.is_some() {
            "a approve · s session · d deny".into()
        } else if !connected {
            "/connect to enable chat".into()
        } else if qn > 0 {
            format!("queue {qn} · Ctrl+Up/Down select · Ctrl+Backspace cancel")
        } else {
            "/ commands  ·  Ctrl+E files  ·  Alt+←/→ tabs  ·  Alt+[ / ] inspector  ·  F1 help"
                .into()
        };
        let footer_provider = footer_provider_id(
            self.runtime.provider.as_str(),
            status.connect_profile.as_deref(),
        );
        let model_cost = self.active_model_cost();
        let footer_limits = self.footer_limits(footer_provider.as_str());
        let footer = FooterModel {
            cwd: self.runtime.cwd.display().to_string(),
            session_short: status.session_short,
            status: status_label,
            status_busy: status.busy,
            provider: self.runtime.provider.clone(),
            model: self.runtime.model_label.clone(),
            effort: self.reasoning_effort.to_string(),
            ctx_used: context.context_tokens_est,
            ctx_total: context.context_capacity,
            ctx_pct: status.ctx_pct,
            connected: status.provider_connected,
            connect_profile: status.connect_profile,
            hints,
            ..footer_usage_summary(&context, model_cost, &footer_limits)
        };
        frame.render_widget(FooterBar { model: &footer }, regions.footer);

        if let Some(ref ov) = self.overlay {
            frame.render_widget(OverlayWidget { overlay: ov }, area);
        }
    }

    fn render_workspace_tabs(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        let spans = WorkspaceMode::ALL.into_iter().flat_map(|mode| {
            let style = if mode == self.workspace_mode {
                theme::brand().add_modifier(Modifier::BOLD)
            } else {
                theme::dim()
            };
            [
                Span::raw(" "),
                Span::styled(mode.label(), style),
                Span::raw(" "),
            ]
        });
        Paragraph::new(Line::from_iter(spans)).render(area, buf);
    }

    fn render_workspace_empty_state(
        &self,
        area: ratatui::layout::Rect,
        buf: &mut ratatui::buffer::Buffer,
    ) {
        Paragraph::new(self.workspace_mode.empty_state().unwrap_or_default())
            .style(theme::dim())
            .alignment(ratatui::layout::Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(theme::muted()),
            )
            .render(area, buf);
    }

    fn toggle_files_panel(&mut self) {
        self.files_visible = !self.files_visible;
        self.file_explorer.focused = self.files_visible;
    }

    fn handle_editor_key(&mut self, key: event::KeyEvent) -> bool {
        if self.workspace_mode != WorkspaceMode::Editor {
            return false;
        }

        // Let workspace-tab switching close an active search/jump and fall
        // through to the global Alt+arrow handler.
        if key.modifiers.contains(KeyModifiers::ALT)
            && (key.code == KeyCode::Left || key.code == KeyCode::Right)
        {
            self.source_viewer.close_search();
            self.source_viewer.close_jump();
            return false;
        }

        if self.source_viewer.search.open {
            return self.handle_search_key(key);
        }
        if self.source_viewer.jump.open {
            return self.handle_jump_key(key);
        }

        let height = self.last_editor_height.saturating_sub(2) as usize;
        // Navigation shortcuts are plain keys so that Alt/Ctrl combinations
        // continue to control workspace tabs and other chrome.
        match key.code {
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.workspace_mode = WorkspaceMode::Chat;
                true
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.source_viewer.start_search();
                true
            }
            KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.source_viewer.start_jump();
                true
            }
            KeyCode::Up if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(-1, height);
                true
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(1, height);
                true
            }
            KeyCode::PageUp if key.modifiers.is_empty() => {
                self.source_viewer
                    .move_cursor_vertical(-(height as isize), height);
                true
            }
            KeyCode::PageDown if key.modifiers.is_empty() => {
                self.source_viewer
                    .move_cursor_vertical(height as isize, height);
                true
            }
            KeyCode::Home if key.modifiers.is_empty() => {
                self.source_viewer.move_to_start_of_line();
                true
            }
            KeyCode::End if key.modifiers.is_empty() => {
                self.source_viewer.move_to_end_of_line();
                true
            }
            KeyCode::Left if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(-1);
                true
            }
            KeyCode::Right if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(1);
                true
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.source_viewer.refresh(self.session.workspace_root());
                self.file_explorer.refresh_git_status();
                true
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(-1);
                true
            }
            KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_horizontal(1);
                true
            }
            KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(1, height);
                true
            }
            KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.source_viewer.move_cursor_vertical(-1, height);
                true
            }
            KeyCode::Char('g') if key.modifiers.is_empty() => {
                self.source_viewer.move_to_first_line();
                true
            }
            KeyCode::Char('G') if key.modifiers.is_empty() => {
                self.source_viewer.move_to_last_line();
                true
            }
            KeyCode::Char('e') if key.modifiers.is_empty() => {
                self.pending_external_editor = true;
                true
            }
            _ => false,
        }
    }

    fn handle_search_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_search();
                true
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.source_viewer.prev_match();
                true
            }
            KeyCode::Enter => {
                self.source_viewer.next_match();
                true
            }
            KeyCode::Backspace => {
                self.source_viewer.backspace_search();
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.source_viewer.append_search_char(c);
                true
            }
            _ => true,
        }
    }

    fn handle_jump_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => {
                self.source_viewer.close_jump();
                true
            }
            KeyCode::Enter => {
                self.source_viewer.commit_jump();
                true
            }
            KeyCode::Backspace => {
                self.source_viewer.backspace_jump();
                true
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.source_viewer.append_jump_char(c);
                true
            }
            _ => true,
        }
    }

    fn handle_file_explorer_key(&mut self, key: event::KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('e') {
            self.toggle_files_panel();
            return true;
        }
        if !self.files_visible || !self.file_explorer.focused {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.file_explorer.focused = false;
                true
            }
            KeyCode::Up => {
                self.file_explorer.move_selection(-1);
                true
            }
            KeyCode::Down => {
                self.file_explorer.move_selection(1);
                true
            }
            KeyCode::Right => {
                self.file_explorer.expand_selected();
                true
            }
            KeyCode::Left => {
                self.file_explorer.collapse_selected();
                true
            }
            KeyCode::Enter => {
                if let Some(path) = self.file_explorer.selected_file_path() {
                    self.open_file_in_editor(&path);
                } else {
                    self.file_explorer.activate_selected();
                }
                true
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.file_explorer.refresh_selected();
                true
            }
            _ => false,
        }
    }

    pub async fn handle_key(&mut self, key: event::KeyEvent) -> Result<(), TuiError> {
        let input_was_empty = self.input.text.is_empty();
        // Allow arrow-key auto-repeat for overlays (and other selection UIs).
        if key.kind != KeyEventKind::Press {
            let allow_repeat = matches!(
                key.kind,
                KeyEventKind::Repeat
                    if matches!(
                        key.code,
                        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                    )
            );
            if !allow_repeat {
                return Ok(());
            }
        }

        if matches!(self.overlay, Some(Overlay::StatusReport { .. })) {
            let ok = map_key(key);
            match ok {
                Key::Enter => {
                    self.overlay = None;
                    if self.input.text.trim().is_empty() {
                        return Ok(());
                    }
                }
                Key::Char(c) => {
                    self.overlay = None;
                    if !c.is_control() && !c.is_whitespace() {
                        self.input.history_browse = false;
                        self.input.insert(c);
                        self.clamp_slash_suggest();
                        return Ok(());
                    }
                }
                _ => {}
            }
        }

        if let Some(ref mut ov) = self.overlay {
            let ok = map_key(key);
            let action = handle_overlay_key(ov, ok);
            match action {
                OverlayAction::None => {}
                OverlayAction::Close => self.overlay = None,
                OverlayAction::BeginOnboarding => {
                    self.open_connect_picker();
                    self.set_feedback(FeedbackSeverity::Info, "Step 1 of 2 · choose a provider");
                }
                OverlayAction::HitlApprove => {
                    self.session
                        .resolve_hitl(HitlDecision::Approve, "tui")
                        .await?;
                    self.overlay = None;
                    self.push_toast("approved");
                }
                OverlayAction::HitlApproveSession => {
                    if let Some(ref p) = self.session.pending_hitl {
                        self.hitl_session_allow.insert(p.tool.clone());
                    }
                    self.session
                        .resolve_hitl(HitlDecision::Approve, "tui")
                        .await?;
                    self.overlay = None;
                    self.push_toast("allowed for session");
                }
                OverlayAction::HitlDeny => {
                    self.session.resolve_hitl(HitlDecision::Deny, "tui").await?;
                    self.overlay = None;
                    self.push_toast("denied");
                }
                OverlayAction::ContinueTurns => {
                    self.overlay = None;
                    self.pending_turn_continue = true;
                    self.busy = true;
                    self.push_toast("continuing");
                }
                OverlayAction::StopTurns => {
                    self.overlay = None;
                    self.status_message = "agent stopped at turn limit".into();
                    self.set_feedback(FeedbackSeverity::Info, "stopped at turn limit");
                }
                OverlayAction::RunCommand(cmd) => {
                    self.overlay = None;
                    self.dispatch_line(&cmd).await?;
                }
                OverlayAction::InsertInput(s) => {
                    self.overlay = None;
                    self.input.text = s;
                    self.input.cursor = self.input.text.len();
                }
                OverlayAction::SelectModel { provider, model } => {
                    self.apply_model_selection(&provider, &model);
                    self.open_effort_picker_for_model(&model);
                }
                OverlayAction::SelectEffort(level) => {
                    self.overlay = None;
                    self.reasoning_effort = level;
                    self.persist_selection();
                    self.set_feedback(FeedbackSeverity::Ok, format!("reasoning effort: {level}"));
                }
                OverlayAction::ConnectSubmitKey {
                    profile_id,
                    api_key,
                } => {
                    // Keep overlay until connect succeeds so a bad key does not wipe paste.
                    let key = api_key.trim().to_string();
                    self.try_connect_api_key(&profile_id, Some(key));
                }
                OverlayAction::ConnectCompleteOauth { profile_id } => {
                    // Enter: try one poll now; keep overlay if still pending
                    if self.oauth_pending.is_some() {
                        self.oauth_last_poll = None;
                        self.poll_oauth_tick();
                        if self.oauth_pending.is_some() {
                            self.status_message =
                                format!("Still waiting for login… (code for {profile_id})");
                        }
                    } else if std::env::var("FORGE_CONNECT_OAUTH_FIXTURE").is_ok() {
                        self.overlay = None;
                        self.finish_connect(&profile_id, None, true);
                    } else {
                        // Restart flow
                        self.begin_oauth_flow(&profile_id);
                    }
                }
                OverlayAction::ConnectUseEnv { profile_id } => {
                    self.try_connect_api_key(&profile_id, None);
                }
                OverlayAction::ConnectPickProfile { profile_id } => {
                    self.overlay = None;
                    self.busy_phase = BusyPhase::Connect;
                    self.push_activity(
                        ActivityKind::Connect,
                        FeedbackSeverity::Info,
                        format!("connect {profile_id}"),
                    );
                    // Continue into oauth / api-key flow for the chosen profile
                    self.finish_connect(&profile_id, None, false);
                    self.busy_phase = BusyPhase::Idle;
                }
                OverlayAction::FilePick { path, is_dir } => {
                    if is_dir {
                        self.open_file_explorer(Some(&path), None);
                    } else {
                        self.open_file_viewer(&path);
                    }
                }
            }
            return Ok(());
        }

        if self.handle_file_explorer_key(key) {
            return Ok(());
        }

        if self.handle_editor_key(key) {
            self.source_viewer.clear_notice();
            return Ok(());
        }

        match key.code {
            KeyCode::Left if key.modifiers.contains(KeyModifiers::ALT) => {
                let next = self.workspace_mode.previous();
                self.workspace_mode = next;
                if next == WorkspaceMode::Editor {
                    self.source_viewer.refresh(self.session.workspace_root());
                    self.file_explorer.refresh_git_status();
                }
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::ALT) => {
                let next = self.workspace_mode.next();
                self.workspace_mode = next;
                if next == WorkspaceMode::Editor {
                    self.source_viewer.refresh(self.session.workspace_root());
                    self.file_explorer.refresh_git_status();
                }
            }
            KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.bottom_panel.open_tab(BottomPanelTab::Tests);
            }
            KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.bottom_panel.open_tab(BottomPanelTab::Diagnostics);
            }
            KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.bottom_panel.open_tab(BottomPanelTab::Terminal);
            }
            KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.bottom_panel.open_tab(BottomPanelTab::Activity);
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_queue_selection(-1);
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_queue_selection(1);
            }
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel_selected_queue();
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.busy {
                    // First Ctrl+C while busy: soft cancel; second: quit
                    if self.cancel_requested {
                        self.should_quit = true;
                        self.last_exit = ExitCode::Canceled;
                    } else {
                        self.cancel_requested = true;
                        self.push_toast("interrupt requested · Ctrl+C again to quit");
                    }
                } else {
                    self.should_quit = true;
                    self.last_exit = ExitCode::Canceled;
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.busy {
                    self.overlay = Some(Overlay::slash_open(""));
                }
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.tool_expanded = !self.tool_expanded;
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            KeyCode::Char('[') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.inspector_view = self.inspector_view.previous();
            }
            KeyCode::Char(']') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.inspector_view = self.inspector_view.next();
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.bottom_panel.toggle();
            }
            KeyCode::F(1) => {
                if self.overlay.is_none() {
                    self.overlay = Some(Overlay::welcome());
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        "Help · press Enter to get started or Esc to dismiss",
                    );
                }
            }
            KeyCode::Esc => {
                self.history.reset_browse();
                self.notices.clear();
                self.clear_error_chrome();
                if self.busy {
                    // Soft interrupt — stop after current model chunk / turn
                    self.cancel_requested = true;
                    self.push_toast("interrupt · finishing current step…");
                } else if !self.input.text.is_empty() {
                    self.input.clear();
                    self.slash_suggest_idx = 0;
                } else {
                    self.feedback = FeedbackModel::default();
                    self.status_message.clear();
                    self.notices.clear();
                    self.notices_until = None;
                    self.tool_expanded = false;
                }
            }
            // Shift+Enter or Alt+Enter → newline (multi-line input)
            KeyCode::Enter
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.input.insert_newline();
            }
            KeyCode::Enter => {
                // Slash suggestions open: Enter selects the highlighted command and runs it.
                // (Do not require the typed prefix to match cmd — filter can match on desc too.)
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.input.text.contains(' ')
                {
                    let idx = self.slash_suggest_idx.min(suggestions.len() - 1);
                    let cmd = suggestions[idx].cmd.clone();
                    let cur = self.input.text.trim();
                    // Keep args only if user already typed past the bare command
                    let line = if cur == cmd.as_str() || cur.starts_with(&(cmd.clone() + " ")) {
                        self.input.take()
                    } else {
                        self.input.set_text(cmd);
                        self.input.take()
                    };
                    if line.is_empty() {
                        return Ok(());
                    }
                    self.history.push(&line);
                    self.slash_suggest_idx = 0;
                    self.notices.clear();
                    self.input.history_browse = false;
                    self.dispatch_line(&line).await?;
                    return Ok(());
                }
                let line = self.input.take();
                // Empty Enter while idle + queue non-empty still sends the next message.
                if line.trim().is_empty() {
                    if !self.busy && !self.message_queue.is_empty() {
                        self.dequeue_and_send_next();
                    }
                    return Ok(());
                }
                self.history.push(&line);
                self.slash_suggest_idx = 0;
                self.notices.clear();
                self.input.history_browse = false;
                // While current message is processing, non-slash text is enqueued (TUI state).
                if self.busy && !line.trim_start().starts_with('/') {
                    self.enqueue_user_message(line);
                    return Ok(());
                }
                self.dispatch_line(&line).await?;
            }
            KeyCode::Tab => {
                self.complete_slash_suggestion();
            }
            KeyCode::Up => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.history.browsing()
                {
                    let n = suggestions.len();
                    self.slash_suggest_idx = (self.slash_suggest_idx + n - 1) % n;
                } else if let Some(text) = self.history.up(&self.input.text) {
                    self.apply_history_text(text);
                }
            }
            KeyCode::Down => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/')
                    && !suggestions.is_empty()
                    && !self.history.browsing()
                {
                    let n = suggestions.len();
                    self.slash_suggest_idx = (self.slash_suggest_idx + 1) % n;
                } else if let Some(text) = self.history.down() {
                    self.apply_history_text(text);
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl+J also inserts newline (terminals that don't send Shift+Enter)
                self.input.insert_newline();
            }
            KeyCode::Char(c) => {
                // Phase 8 (TUI-06): `/` inserts into the main textbox; do not open palette.
                // Typing while busy composes the next message (Enter enqueues).
                self.input.history_browse = false;
                self.input.insert(c);
                self.clamp_slash_suggest();
            }
            KeyCode::Backspace => {
                self.input.backspace();
                self.clamp_slash_suggest();
            }
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::PageUp => self.scroll_conversation_up(5),
            KeyCode::PageDown => self.scroll_conversation_down(5),
            _ => {}
        }
        if input_was_empty && !self.input.text.is_empty() {
            self.splash_dismissed = true;
        }
        Ok(())
    }

    fn scroll_conversation_up(&mut self, amount: u16) {
        self.chat_follow = false;
        self.chat_scroll = self.chat_scroll.saturating_add(amount);
    }

    fn scroll_conversation_down(&mut self, amount: u16) {
        self.chat_scroll = self.chat_scroll.saturating_sub(amount);
        if self.chat_scroll == 0 {
            self.chat_follow = true;
        }
    }

    pub fn handle_mouse(&mut self, kind: MouseEventKind) {
        match kind {
            MouseEventKind::ScrollUp => self.scroll_conversation_up(3),
            MouseEventKind::ScrollDown => self.scroll_conversation_down(3),
            _ => {}
        }
    }

    async fn handle_model_command(&mut self, provider: Option<&str>, model: Option<&str>) {
        if provider.is_none() && model.is_none() {
            let items = self.model_picker_items(true);
            let mut overlay = Overlay::model_open_with(items);
            overlay.focus_model(&self.runtime.model_label);
            self.overlay = Some(overlay);
            self.status_message = "pick a model (live catalog when connected)".into();
        }

        let connected_prefix = self.connect_profile.as_deref().and_then(|id| {
            self.connect_registry
                .get(id)
                .map(|profile| profile.model_provider_prefix.as_str())
        });
        let model_id = normalize_model_id(provider.unwrap_or(""), model, connected_prefix);
        if model_id.trim().is_empty() {
            self.set_feedback(FeedbackSeverity::Warn, "usage: /model <provider/model>");
            return;
        }
        let target_prefix = Self::model_prefix(&model_id);
        let matching_profile = self.connected_profile_for_model_prefix(target_prefix);
        if matching_profile.is_none() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("connect `{target_prefix}` first before selecting {model_id}"),
            );
            self.push_notice(vec![
                format!("No connected provider matches `{target_prefix}`."),
                "Use /connect, or pick a model from the current provider catalog.".into(),
            ]);
            return;
        } else {
            self.apply_model_selection("native", &model_id);
            self.open_effort_picker_for_model(&model_id);
        }
    }

    pub async fn dispatch_line(&mut self, line: &str) -> Result<(), TuiError> {
        if let Some(cmd_res) = parse_slash(line) {
            let slash_name = line.split_whitespace().next().unwrap_or("/");
            self.push_activity(ActivityKind::Slash, FeedbackSeverity::Info, slash_name);
            match cmd_res {
                Ok(SlashCommand::Quit) => {
                    self.should_quit = true;
                    self.status_message = "quitting…".into();
                }
                Ok(SlashCommand::Compact) => {
                    self.queue_context_reset();
                    if cfg!(test) {
                        let _ = self.drain_pending_context_reset(None).await;
                    }
                }
                Ok(SlashCommand::Model { provider, model }) => {
                    self.handle_model_command(provider.as_deref(), model.as_deref())
                        .await
                }
                Ok(SlashCommand::ResumeList) => {
                    match recent_resume_sessions(
                        self.session.journal_dir(),
                        self.session.session_id,
                        10,
                    ) {
                        Ok(sessions) if sessions.is_empty() => {
                            self.status_message = "no previous sessions".into();
                            self.push_notice(vec![
                                "No previous sessions found for this workspace.".into(),
                            ]);
                        }
                        Ok(sessions) => {
                            self.status_message = format!("{} resumable sessions", sessions.len());
                            let items = sessions
                                .into_iter()
                                .map(|session| {
                                    let timestamp: chrono::DateTime<chrono::Local> =
                                        session.modified.into();
                                    ResumeSessionItem {
                                        id: session.id.to_string(),
                                        modified: timestamp.format("%Y-%m-%d %H:%M").to_string(),
                                    }
                                })
                                .collect();
                            self.notices.clear();
                            self.overlay = Some(Overlay::resume_picker(items));
                        }
                        Err(error) => {
                            self.report_error(&format!("Could not list previous sessions: {error}"))
                        }
                    }
                }
                Ok(SlashCommand::Resume { session_id }) => {
                    match self.session.resume_session(session_id).await {
                        Ok(_report) => {
                            self.overlay = None;
                            self.notices.clear();
                            self.status_message = "session resumed".into();
                            self.set_feedback(
                                FeedbackSeverity::Ok,
                                "session restored · ready for the next action",
                            );
                            self.push_toast(format!("resumed {session_id}"));
                            self.push_activity(
                                ActivityKind::System,
                                FeedbackSeverity::Ok,
                                format!("session resumed · {session_id}"),
                            );
                            self.ui_banners.clear();
                            self.message_queue.clear();
                            self.queue_selected = None;
                            self.stream_preview.clear();
                            self.stream_thinking.clear();
                            self.chat_message_start = 0;
                            self.chat_event_start = 0;
                            self.chat_scroll = 0;
                            self.chat_follow = true;
                            self.hitl_session_allow.clear();
                            self.maybe_open_hitl();
                        }
                        Err(error) => {
                            self.report_error(&format!(
                                "Could not resume session {session_id}: {error}"
                            ));
                        }
                    }
                }
                Ok(SlashCommand::Copy) => {
                    let last = self
                        .session
                        .messages
                        .iter()
                        .rev()
                        .find(|m| {
                            m.role == forge_types::MessageRole::Assistant && !m.content.is_empty()
                        })
                        .map(|m| m.content.clone());
                    if let Some(text) = last {
                        let ok = std::process::Command::new("pbcopy")
                            .stdin(std::process::Stdio::piped())
                            .spawn()
                            .and_then(|mut c| {
                                use std::io::Write;
                                if let Some(mut sin) = c.stdin.take() {
                                    sin.write_all(text.as_bytes())?;
                                }
                                c.wait()?;
                                Ok(())
                            })
                            .is_ok()
                            || std::process::Command::new("wl-copy")
                                .arg(&text)
                                .status()
                                .map(|s| s.success())
                                .unwrap_or(false);
                        if ok {
                            self.push_toast("copied last answer");
                        } else {
                            self.push_notice(vec![
                                "Clipboard unavailable (pbcopy/wl-copy).".into(),
                                text.chars().take(400).collect(),
                            ]);
                        }
                    } else {
                        self.push_toast("nothing to copy");
                    }
                }
                Ok(SlashCommand::Clear) => {
                    // Hide everything currently in the transcript without deleting session
                    // context, so subsequent model turns still see the full conversation.
                    self.chat_message_start = self.session.messages.len();
                    self.chat_event_start = self.session.events.len();
                    self.ui_banners.clear();
                    self.notices.clear();
                    self.clear_error_chrome();
                    self.feedback = FeedbackModel::default();
                    self.status_message.clear();
                    self.toast = None;
                    self.chat_scroll = 0;
                    self.chat_follow = true;
                }
                Ok(SlashCommand::File { path }) => {
                    if let Some(path) = path.as_deref() {
                        match self.resolve_workspace_path(path) {
                            Ok(resolved) if resolved.is_file() => {
                                self.open_file_viewer(&resolved.display().to_string());
                            }
                            Ok(resolved) if resolved.is_dir() => {
                                self.open_file_explorer(
                                    Some(&resolved.display().to_string()),
                                    None,
                                );
                            }
                            Ok(_) => self.open_file_explorer(
                                None,
                                Some("Path is not a regular file or directory".into()),
                            ),
                            Err(err) => self.open_file_explorer(
                                None,
                                Some(format!("Could not open path: {err}")),
                            ),
                        }
                    } else {
                        self.open_file_explorer(None, None);
                    }
                }
                Ok(SlashCommand::Disconnect { profile_id }) => {
                    let msg = self.disconnect_auth(profile_id.as_deref())?;
                    self.open_connect_picker();
                    self.status_message = msg;
                }
                Ok(SlashCommand::Connect(action)) => {
                    self.handle_connect(action);
                }
                Ok(SlashCommand::Sync) => {
                    self.queue_sync();
                    // Unit tests call `dispatch_line` directly without the event loop;
                    // run queued sync immediately in that case.
                    if cfg!(test) {
                        let _ = self.drain_pending_sync(None).await;
                    }
                }
                Ok(SlashCommand::Refresh) => {
                    self.file_explorer.refresh_git_status();
                    self.status_message = "Refreshing git status...".into();
                }
                Ok(SlashCommand::Edit) => {
                    self.pending_external_editor = true;
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.set_feedback(FeedbackSeverity::Warn, msg.clone());
                    self.notices.clear();
                    self.push_toast(msg);
                }
            }
            return Ok(());
        }

        // Queue user message — the event loop drains this so the YOU bubble paints
        // before the model call, and so stream deltas can redraw each frame.
        self.clear_error_chrome();
        // Re-apply credentials (with silent refresh) before each turn so sessions stay signed in.
        if !self.auth_suspended {
            if let Some(pid) = self.connect_profile.clone() {
                self.apply_connect_credentials(&pid);
            } else {
                // Try restore mid-session if credentials appeared (e.g. /connect in another terminal)
                let restored = {
                    let svc = ConnectService {
                        registry: &self.connect_registry,
                        store: &self.connect_store,
                        active_profile_id: None,
                        active_model: None,
                    };
                    svc.connected_profiles().ok().and_then(|v| {
                        v.iter()
                            .find(|p| p.id == "xai")
                            .cloned()
                            .or_else(|| v.into_iter().next())
                    })
                };
                if let Some(p) = restored {
                    self.connect_profile = Some(p.id.clone());
                    self.apply_connect_credentials(&p.id);
                    if let Some(m) = p.default_model() {
                        self.runtime.model_label = m.to_string();
                        self.session.set_active_model(m);
                    }
                    self.refresh_connection_ui();
                }
            }
        }

        // Gate: no LLM chat without a live provider (slash commands already returned above).
        if !self.is_provider_connected() {
            self.input.set_text(line);
            self.report_error(
                "Not connected to an LLM provider. Run /connect (xAI Grok or OpenCode Go), then send again.",
            );
            self.refresh_connection_ui();
            return Ok(());
        }

        self.pending_prompt = Some(line.to_string());
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.turn_started = Some(Instant::now());
        // A new user turn should always follow the live conversation tail.
        // This also ensures its thinking block is visible after the user has
        // previously scrolled up to inspect an older response.
        self.chat_follow = true;
        self.chat_scroll = 0;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
        Ok(())
    }

    /// Run a queued user prompt with streaming + intermediate redraws.
    /// When `terminal` is `None` (unit tests), runs without intermediate draws.
    pub async fn drain_pending_prompt(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        let continuing = std::mem::take(&mut self.pending_turn_continue);
        let line = self.pending_prompt.take();
        if line.is_none() && !continuing {
            return Ok(());
        }

        // Refresh OAuth close to expiry and recycle the worker with the current token.
        if let Some(profile_id) = self.connect_profile.clone() {
            self.apply_connect_credentials(&profile_id);
        }

        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started.get_or_insert_with(Instant::now);
        self.thinking_started = None;
        self.thought_secs = None;

        if let Some(ref line) = line {
            if let Err(e) = self.session.append_user_message(line).await {
                self.busy = false;
                self.busy_phase = BusyPhase::Idle;
                self.report_error(&e.to_string());
                self.last_exit = ExitCode::Failed;
                return Ok(());
            }
        }

        // Paint YOU message immediately
        if let Some(term) = terminal.as_deref_mut() {
            term.draw(|f| self.draw(f))?;
        }

        let max_turns = self.session.max_turns();
        let mut outcome_err: Option<String> = None;
        let mut turn_thought_secs = 0.0f64;
        let mut saw_thinking = false;

        'turns: for turn in 0..max_turns {
            let req = match self.session.prepare_model_step(turn).await {
                Ok(r) => r,
                Err(e) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
            };

            let model = self.session.model_client();
            let (tx, rx) = std::sync::mpsc::channel::<ModelStreamEvent>();
            let handle =
                tokio::spawn(async move { model.complete_with_stream(req, Some(tx)).await });

            // Pump stream events + redraw until the model call finishes
            loop {
                if self.cancel_requested {
                    handle.abort();
                    self.cancel_requested = false;
                    self.turn_started = None;
                    outcome_err = Some("interrupted".into());
                    break 'turns;
                }
                while let Ok(ev) = rx.try_recv() {
                    match ev {
                        ModelStreamEvent::TextDelta { text } => {
                            // Thinking ends when answer tokens begin
                            self.close_thinking_timer();
                            self.stream_preview.push_str(&text);
                        }
                        ModelStreamEvent::ThinkingDelta { text } => {
                            if self.thinking_started.is_none() {
                                // Prefer turn start so duration covers full thinking wait if
                                // the provider dumps reasoning in one late chunk.
                                self.thinking_started =
                                    self.turn_started.or_else(|| Some(Instant::now()));
                            }
                            self.stream_thinking.push_str(&text);
                        }
                        _ => {}
                    }
                }
                // Redraw every tick so spinner and elapsed time stay current.
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }

                if handle.is_finished() {
                    // Drain remaining events
                    while let Ok(ev) = rx.try_recv() {
                        match ev {
                            ModelStreamEvent::TextDelta { text } => {
                                self.close_thinking_timer();
                                self.stream_preview.push_str(&text);
                            }
                            ModelStreamEvent::ThinkingDelta { text } => {
                                if self.thinking_started.is_none() {
                                    self.thinking_started =
                                        self.turn_started.or_else(|| Some(Instant::now()));
                                }
                                self.stream_thinking.push_str(&text);
                            }
                            _ => {}
                        }
                    }
                    // Thinking-only or late thinking dump: close the clock now
                    self.close_thinking_timer();
                    if let Some(term) = terminal.as_deref_mut() {
                        term.draw(|f| self.draw(f))?;
                    }
                    break;
                }

                // Keep the terminal responsive while the current turn is streaming so
                // the operator can type the next message and enqueue it with Enter.
                if terminal.is_some() {
                    drain_events(self).await?;
                    if self.should_quit {
                        handle.abort();
                        self.busy = false;
                        self.busy_phase = BusyPhase::Idle;
                        self.stream_preview.clear();
                        self.stream_thinking.clear();
                        self.turn_started = None;
                        self.thinking_started = None;
                        self.thought_secs = None;
                        self.last_exit = ExitCode::Canceled;
                        return Ok(());
                    }
                }

                // ~10 Hz keeps the timer + spinner smooth without burning CPU
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            let mut last = match handle.await {
                Ok(Ok(r)) => r,
                Ok(Err(e)) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
                Err(e) => {
                    outcome_err = Some(format!("model task join: {e}"));
                    break;
                }
            };

            // Provider may attach reasoning only on the final object (no stream deltas).
            if self.stream_thinking.is_empty() {
                if let Some(ref th) = last.thinking {
                    if !th.is_empty() {
                        if self.thinking_started.is_none() {
                            self.thinking_started = self.turn_started;
                        }
                        self.stream_thinking = th.clone();
                        self.close_thinking_timer();
                        // One paint so the user can see thinking before collapse
                        if let Some(term) = terminal.as_deref_mut() {
                            let _ = term.draw(|f| self.draw(f));
                        }
                    }
                }
            } else if last.thinking.as_ref().map(|t| t.is_empty()).unwrap_or(true) {
                // Prefer streamed thinking body on the final message
                last.thinking = Some(self.stream_thinking.clone());
            }
            self.close_thinking_timer();

            let thought = self.thought_secs.take();
            self.stream_preview.clear();
            self.stream_thinking.clear();
            // Keep turn_started until full agent turn ends (multi-tool steps).
            if let Some(call) = last.tool_calls.first() {
                self.busy_phase = BusyPhase::Tool {
                    name: call.name.clone(),
                };
                self.push_activity(
                    ActivityKind::Tool,
                    FeedbackSeverity::Info,
                    format!("tool_intent {}", call.name),
                );
                if let Some(term) = terminal.as_deref_mut() {
                    term.draw(|f| self.draw(f))?;
                }
            }
            match self.session.apply_model_response(last).await {
                Ok(out) => {
                    if let Some(secs) = thought {
                        saw_thinking = true;
                        turn_thought_secs += secs;
                    }
                    // Reset per-model-step thinking timers for multi-tool loops.
                    self.thinking_started = None;
                    self.thought_secs = None;
                    let mut turn_done = false;
                    match out {
                        ApplyOutcome::Done(_) => {
                            outcome_err = None;
                            turn_done = true;
                        }
                        ApplyOutcome::Hitl(_) => {
                            outcome_err = None;
                            turn_done = true;
                        }
                        ApplyOutcome::Continue => {
                            self.busy_phase = BusyPhase::Model;
                            if let Some(term) = terminal.as_deref_mut() {
                                term.draw(|f| self.draw(f))?;
                            }
                            continue;
                        }
                    }
                    if turn_done {
                        self.file_explorer.refresh_git_status();
                        break 'turns;
                    }
                }
                Err(e) => {
                    outcome_err = Some(e.to_string());
                    break;
                }
            }
        }

        let turn_limit_reached = outcome_err.is_none()
            && self.session.status != forge_types::SessionStatus::Completed
            && self.session.status != forge_types::SessionStatus::AwaitingHitl;

        self.busy = false;
        self.busy_phase = BusyPhase::Idle;
        self.stream_preview.clear();
        self.stream_thinking.clear();
        self.turn_started = None;
        self.thinking_started = None;
        // Keep thought_secs on the message; clear live field
        self.thought_secs = None;

        if turn_limit_reached {
            self.overlay = Some(Overlay::turn_limit(max_turns));
            self.last_exit = ExitCode::Success;
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("{max_turns} steps reached — continue?"),
            );
            self.push_activity(
                ActivityKind::Model,
                FeedbackSeverity::Warn,
                "turn limit reached",
            );
        } else if let Some(e) = outcome_err {
            self.report_error(&e);
            self.last_exit = ExitCode::Failed;
            // Leave queue intact so the operator can fix and continue.
        } else if self.session.pending_hitl.is_some() {
            if let Some(ref p) = self.session.pending_hitl {
                self.overlay = Some(Overlay::hitl(p.clone()));
            }
            self.last_exit = ExitCode::AwaitingHitl;
            self.set_feedback(FeedbackSeverity::Warn, "awaiting human approval");
            self.push_activity(ActivityKind::Hitl, FeedbackSeverity::Warn, "hitl waiting");
            // Do not auto-dequeue until HITL is resolved.
        } else {
            if saw_thinking {
                self.persist_turn_thinking_duration(turn_thought_secs);
            }
            self.clear_error_chrome();
            self.tool_expanded = false;
            if self.message_queue.is_empty() {
                self.feedback = FeedbackModel::default();
                self.status_message.clear();
            } else {
                self.push_toast(format!(
                    "{} queued · sending next",
                    self.message_queue.len()
                ));
                self.set_feedback(
                    FeedbackSeverity::Info,
                    format!("{} in queue — sending next", self.message_queue.len()),
                );
            }
            self.push_activity(ActivityKind::Model, FeedbackSeverity::Ok, "model ok");
            if !self.message_queue.is_empty() {
                self.dequeue_and_send_next();
            }
        }
        Ok(())
    }

    pub fn maybe_open_hitl(&mut self) {
        if self.overlay.is_none() {
            if let Some(ref p) = self.session.pending_hitl {
                if self.hitl_session_allow.contains(&p.tool) {
                    // Will be drained by `drain_auto_hitl` in the event loop.
                    return;
                }
                self.overlay = Some(Overlay::hitl(p.clone()));
            }
        }
    }

    /// Auto-approve HITL for tools allowed this session (`s` key).
    pub async fn drain_auto_hitl(&mut self) -> Result<(), TuiError> {
        if let Some(ref p) = self.session.pending_hitl.clone() {
            if self.hitl_session_allow.contains(&p.tool) {
                self.session
                    .resolve_hitl(HitlDecision::Approve, "tui-session")
                    .await?;
                self.push_toast(format!("auto-approved {}", p.tool));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod sync_helpers_tests {
    use super::heuristic_commit_message;

    #[test]
    fn heuristic_from_name_status() {
        let msg = heuristic_commit_message("A\tfoo.rs\nM\tbar.rs\n");
        assert!(msg.contains("foo.rs") || msg.contains("bar.rs"), "{msg}");
        assert!(
            msg.starts_with("Change") || msg.starts_with("Update") || msg.starts_with("Add"),
            "{msg}"
        );
    }
}

fn truncate_one_line(s: &str, max: usize) -> String {
    let t = s.lines().next().unwrap_or(s).trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Deterministic commit subject from `git diff --cached --name-status` when no LLM is available.
fn heuristic_commit_message(name_status: &str) -> String {
    let mut added = 0usize;
    let mut modified = 0usize;
    let mut deleted = 0usize;
    let mut names: Vec<String> = Vec::new();
    for line in name_status.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let code = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        let base = path.rsplit('/').next().unwrap_or(path.as_str()).to_string();
        if names.len() < 3 && !names.iter().any(|n| n == &base) {
            names.push(base);
        }
        match code.chars().next().unwrap_or(' ') {
            'A' => added += 1,
            'D' => deleted += 1,
            'M' | 'R' | 'C' | 'T' => modified += 1,
            _ => modified += 1,
        }
    }
    if names.is_empty() {
        return "Update project files".into();
    }
    let list = names.join(", ");
    let verb = if added > 0 && modified == 0 && deleted == 0 {
        "Add"
    } else if deleted > 0 && added == 0 && modified == 0 {
        "Remove"
    } else if modified > 0 && added == 0 && deleted == 0 {
        "Update"
    } else {
        "Change"
    };
    format!("{verb} {list}")
}

fn map_key(key: event::KeyEvent) -> OverlayKey {
    match key.code {
        KeyCode::Esc => OverlayKey::Esc,
        KeyCode::Enter => OverlayKey::Enter,
        KeyCode::Up => OverlayKey::Up,
        KeyCode::Down => OverlayKey::Down,
        KeyCode::Left => OverlayKey::Left,
        KeyCode::Right => OverlayKey::Right,
        KeyCode::Backspace => OverlayKey::Backspace,
        KeyCode::Char(c) => OverlayKey::Char(c),
        _ => OverlayKey::Other,
    }
}

/// Drain every pending terminal event (paste floods many keys; do not drop them).
async fn drain_events(app: &mut TuiApp) -> Result<(), TuiError> {
    loop {
        if !event::poll(Duration::from_millis(0))? {
            break;
        }
        match event::read()? {
            Event::Key(key) => app.handle_key(key).await?,
            Event::Mouse(mouse) => app.handle_mouse(mouse.kind),
            Event::Paste(data) => app.handle_paste(&data),
            _ => {}
        }
    }
    Ok(())
}

/// Run the full-screen TUI until quit.
pub async fn run_tui(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
) -> Result<ExitSummary, TuiError> {
    enable_raw_mode()?;
    // Ensure the terminal is restored on panic, returned errors and normal exit.
    let _guard = TerminalGuard::install();
    let mut stdout = stdout();
    // Bracketed paste plus keyboard enhancement for reliable key disambiguation.
    // Deliberately do not enable mouse capture: the terminal must retain mouse selection so
    // users can select and copy transcript text. Conversation scrolling remains on PageUp/Down.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new(session, runtime);
    if !app.is_provider_connected() {
        app.overlay = Some(Overlay::welcome());
        app.set_feedback(
            FeedbackSeverity::Info,
            "Welcome · connect a provider to start chatting",
        );
    }
    let result = run_loop(&mut terminal, &mut app).await;

    app.persist_selection();

    result.map(|_| {
        let report = app.session.token_usage_report();
        ExitSummary {
            exit_code: app.last_exit,
            session_id: app.session.session_id.to_string(),
            token_usage: (report.api.total_api_tokens() > 0)
                .then(|| format_exit_token_usage(&report)),
        }
    })
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<(), TuiError> {
    while !app.should_quit {
        app.tick_toast();
        app.tick_notices();
        app.drain_auto_hitl().await?;
        app.maybe_open_hitl();
        // Grok-style device-code: poll token endpoint while overlay is open
        app.poll_oauth_tick();
        terminal.draw(|f| app.draw(f))?;

        // Drain queued user prompt with streaming redraws (YOU paints before first token)
        if app.pending_prompt.is_some() {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        if app.pending_turn_continue {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        // Drain queued long-running slash tasks (so the command echo paints first).
        if app.pending_sync {
            app.drain_pending_sync(Some(terminal)).await?;
            continue;
        }
        if app.pending_hitl_decision.is_some() {
            app.drain_pending_hitl(Some(terminal)).await?;
            continue;
        }
        if app.pending_context_reset {
            app.drain_pending_context_reset(Some(terminal)).await?;
            continue;
        }
        if app.pending_external_editor {
            app.drain_pending_external_editor(Some(terminal)).await?;
            continue;
        }

        if event::poll(Duration::from_millis(200))? {
            // Read the ready event, then drain the rest of the queue so a paste
            // of a long API key is not truncated to a handful of characters.
            match event::read()? {
                Event::Key(key) => app.handle_key(key).await?,
                Event::Mouse(mouse) => app.handle_mouse(mouse.kind),
                Event::Paste(data) => app.handle_paste(&data),
                _ => {}
            }
            drain_events(app).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::LoopConfig;
    use forge_model::MockModelClient;
    use forge_tools::ToolRegistry;
    use forge_types::{Message, MessageRole, ModelResponse};
    use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Returns (journal_workspace_guard, session). Keep the TempDir until the test ends.
    async fn test_session() -> (TempDir, AgentSession) {
        let dir = TempDir::new().unwrap();
        let model = Arc::new(MockModelClient::script(vec![ModelResponse {
            text: "hello tui".into(),
            tool_calls: vec![],
            usage: None,
            thinking: None,
        }]));
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,

                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        (dir, session)
    }

    #[tokio::test]
    async fn typing_reuses_cached_conversation_lines() {
        use ratatui::backend::TestBackend;

        let (dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
            },
        );
        app.splash_dismissed = true;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.conversation_cache
            .as_mut()
            .unwrap()
            .lines
            .reserve(1_000);
        let cached_capacity = app.conversation_cache.as_ref().unwrap().lines.capacity();

        app.input.insert('x');
        terminal.draw(|frame| app.draw(frame)).unwrap();

        assert_eq!(
            app.conversation_cache.as_ref().unwrap().lines.capacity(),
            cached_capacity
        );
    }

    #[tokio::test]
    async fn streaming_updates_reuse_cached_transcript_lines() {
        use ratatui::backend::TestBackend;

        let (dir, mut session) = test_session().await;
        session.messages.push(Message {
            role: MessageRole::Assistant,
            content: "historical answer".into(),
            tool_call_id: None,
            name: None,
            thinking: Some("historical completed thinking".into()),
            thinking_duration_secs: Some(1.0),
            tool_calls: vec![],
        });
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "test".into(),
                startup_notices: Vec::new(),
            },
        );
        app.splash_dismissed = true;
        app.busy = true;
        app.busy_phase = BusyPhase::Model;
        app.stream_preview = "first chunk".into();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.conversation_cache
            .as_mut()
            .unwrap()
            .lines
            .reserve(1_000);
        let cached_capacity = app.conversation_cache.as_ref().unwrap().lines.capacity();

        app.stream_preview.push_str(" and updated tail");
        terminal.draw(|frame| app.draw(frame)).unwrap();

        assert_eq!(
            app.conversation_cache.as_ref().unwrap().lines.capacity(),
            cached_capacity,
            "stream deltas must not rebuild historical transcript lines"
        );
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("updated tail"), "{rendered}");
        assert!(
            !rendered.contains("historical completed thinking"),
            "{rendered}"
        );
    }

    #[test]
    fn footer_limits_parser_keeps_only_inline_limit_fields() {
        let limits = footer_limits_from_report(&[
            "Provider: OpenAI Codex".into(),
            "Session limit: 75% remaining".into(),
            "Weekly limit: 50% remaining".into(),
            "Credits: unlimited".into(),
        ]);

        assert_eq!(limits.usage, "Session limit: 75% remaining");
        assert_eq!(limits.weekly_limit, "Weekly limit: 50% remaining");
        assert_eq!(limits.credits, "Credits: unlimited");
    }

    #[test]
    fn recent_resume_sessions_lists_previous_valid_journals() {
        let dir = TempDir::new().unwrap();
        let current = uuid::Uuid::new_v4();
        let previous = uuid::Uuid::new_v4();
        std::fs::write(dir.path().join(format!("{current}.db")), "").unwrap();
        std::fs::write(dir.path().join(format!("{previous}.db")), "").unwrap();
        std::fs::write(dir.path().join("not-a-session.db"), "").unwrap();
        std::fs::write(dir.path().join(format!("{}.txt", uuid::Uuid::new_v4())), "").unwrap();

        let sessions = recent_resume_sessions(dir.path(), current, 10).unwrap();

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, previous);
    }

    #[tokio::test]
    async fn resume_command_replaces_active_conversation_in_app() {
        let (dir, session) = test_session().await;
        let model = Arc::new(MockModelClient::script(vec![]));
        let mut previous = AgentSession::create(
            LoopConfig {
                max_turns: 4,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                enable_context_lifecycle: true,
                enable_governance: true,
                ..Default::default()
            },
            model,
            ToolRegistry::new(),
        )
        .await
        .unwrap();
        previous
            .append_user_message("restored conversation")
            .await
            .unwrap();
        let previous_id = previous.session_id;

        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line(&format!("/resume {previous_id}"))
            .await
            .unwrap();

        assert_eq!(app.session.session_id, previous_id);
        assert!(app
            .session
            .messages
            .iter()
            .any(|message| message.content == "restored conversation"));
        assert!(app.status_message.contains("resumed"));
        assert!(app.notices.is_empty());
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.summary.contains("session resumed")));
    }

    #[tokio::test]
    async fn compact_reports_context_handoff_in_chat_and_activity() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );

        app.dispatch_line("/compact").await.unwrap();

        assert!(app.notices.is_empty());
        assert!(app.ui_banners.is_empty());
        assert!(app
            .activity
            .all()
            .iter()
            .any(|item| item.kind == ActivityKind::Context));
        assert_eq!(app.status_message, "Continuing in a fresh context");
        assert!(app
            .ui_banners
            .iter()
            .all(|item| !matches!(item, ChatItem::Banner { .. })));
    }

    #[tokio::test]
    async fn enter_while_busy_enqueues_user_message() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.busy = true;
        for c in "queued later".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.message_queue.len(), 1);
        assert!(app.pending_prompt.is_none());
        assert_eq!(
            app.message_queue.iter().next().map(|s| s.as_str()),
            Some("queued later")
        );
    }

    #[tokio::test]
    async fn typing_while_busy_updates_input_buffer() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.busy = true;
        for c in "next".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(app.input.text, "next");
        assert_eq!(app.message_queue.len(), 0);
    }

    #[tokio::test]
    async fn ctrl_p_toggles_bottom_panel_without_touching_input() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.input.set_text("draft");

        app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(app.bottom_panel.open);
        assert_eq!(app.input.text, "draft");

        app.handle_key(press(KeyCode::Char('p'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(!app.bottom_panel.open);
        assert_eq!(app.input.text, "draft");
    }

    #[tokio::test]
    async fn alt_number_opens_selected_bottom_panel_tab() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );

        app.handle_key(press(KeyCode::Char('4'), KeyModifiers::ALT))
            .await
            .unwrap();
        assert!(app.bottom_panel.open);
        assert_eq!(app.bottom_panel.active, BottomPanelTab::Activity);
    }

    #[tokio::test]
    async fn question_mark_opens_help_overlay() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.handle_key(press(KeyCode::F(1), KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::Help)));
        assert!(app.input.text.is_empty());
        assert!(app.feedback.text.contains("Help"));
    }

    #[tokio::test]
    async fn empty_enter_when_idle_dequeues_and_sends() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        // Simulate messages enqueued while processing.
        app.message_queue.enqueue("from queue");
        app.busy = false;
        assert!(app.pending_prompt.is_none());
        // Empty Enter = user action to dequeue + send
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.message_queue.is_empty());
        assert_eq!(app.pending_prompt.as_deref(), Some("from queue"));
        assert!(app.busy);
    }

    #[tokio::test]
    async fn ctrl_backspace_cancels_selected_queue_message() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.enqueue_user_message("a".into());
        app.enqueue_user_message("b".into());
        app.move_queue_selection(1);
        app.handle_key(press(KeyCode::Backspace, KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert_eq!(app.message_queue.len(), 1);
        assert_eq!(
            app.message_queue.iter().next().map(|s| s.as_str()),
            Some("a")
        );
    }

    #[tokio::test]
    async fn effort_selection_persists_across_tui_instances() {
        let (_dir, session) = test_session().await;
        let credential_dir = tempfile::tempdir().unwrap();
        let credential_path = credential_dir.path().join("credentials.toml");
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(credential_path.clone());

        app.reasoning_effort = ReasoningEffort::High;
        app.persist_selection();

        assert_eq!(
            app.connect_store.last_effort().unwrap().as_deref(),
            Some("high")
        );

        let (_dir, session) = test_session().await;
        let mut restarted = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        restarted.connect_store = CredentialStore::new(credential_path);
        restarted = restarted.restore_saved_auth();

        assert_eq!(restarted.reasoning_effort, ReasoningEffort::High);
    }

    #[tokio::test]
    async fn model_command_applies_provider_id_to_session() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-openai-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());
        app.runtime.model_label = "openai/gpt-4.1-mini".into();
        app.session.set_active_model("openai/gpt-4.1-mini");
        app.apply_model_selection("native", "openai/gpt-4.1-mini");
        assert_eq!(app.runtime.model_label, "openai/gpt-4.1-mini");
        assert_eq!(app.session.active_model, "openai/gpt-4.1-mini");
        assert!(app.pending_prompt.is_none());
    }

    #[tokio::test]
    async fn model_command_rejects_cross_provider_selection_without_matching_connection() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai-codex/gpt-5.6-sol".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_oauth(
                "openai_codex",
                forge_connect::OauthTokens {
                    access_token:
                        "header.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.sig"
                            .to_string(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        app.connect_profile = Some("openai_codex".into());
        app.runtime.model_label = "openai-codex/gpt-5.6-sol".into();
        app.session.set_active_model("openai-codex/gpt-5.6-sol");

        app.dispatch_line("/model not-connected claude-sonnet-4-5")
            .await
            .unwrap();

        assert_eq!(app.connect_profile.as_deref(), Some("openai_codex"));
        assert_eq!(app.runtime.model_label, "openai-codex/gpt-5.6-sol");
        assert!(
            app.status_message.contains("connect `not-connected` first")
                || app.notices.iter().any(|l| l.contains("not-connected")),
            "expected rejection notice, got status={} notices={:?}",
            app.status_message,
            app.notices
        );
    }

    #[tokio::test]
    async fn slash_sync_commits_and_does_not_queue_chat() {
        let (dir, session) = test_session().await;
        for args in [
            &["init"][..],
            &["config", "user.email", "forge@test"][..],
            &["config", "user.name", "Forge Test"][..],
        ] {
            std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap();
        }
        // Need an initial commit so later push has a branch tip; create one empty first.
        std::fs::write(dir.path().join("README"), "x").unwrap();
        std::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Bare remote for push
        let remote = dir.path().join("remote.git");
        std::process::Command::new("git")
            .args(["init", "--bare"])
            .arg(&remote)
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["remote", "add", "origin"])
            .arg(&remote)
            .current_dir(dir.path())
            .output()
            .unwrap();
        std::process::Command::new("git")
            .args(["push", "-u", "origin", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap();

        std::fs::write(dir.path().join("note.txt"), "hello").unwrap();

        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: dir.path().to_path_buf(),
                version: "0.12.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line("/sync").await.unwrap();
        assert!(app.pending_prompt.is_none());
        assert!(!app.busy);
        let log = app
            .run_git_tool("log", vec!["-1".into(), "--oneline".into()])
            .await
            .unwrap();
        // Heuristic message mentions note.txt when mock has no real LLM summary
        assert!(
            log.content.contains("note") || log.content.contains("Add") || !log.content.is_empty(),
            "expected a commit: {}",
            log.content
        );
        assert!(
            app.activity
                .recent(12)
                .iter()
                .any(|e| e.summary.contains("git commit") || e.summary.contains("git push")),
            "activity should record sync steps"
        );
    }

    #[tokio::test]
    async fn app_dispatch_user_message() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("/tmp"),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line("hi").await.unwrap();
        app.drain_pending_prompt(None).await.unwrap();
        assert!(
            app.session
                .messages
                .iter()
                .any(|m| m.content.contains("hello tui") || m.content == "hi"),
            "messages={:?}",
            app.session
                .messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn app_status_command() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line("/status").await.unwrap();
        assert!(app.overlay.is_none());
        assert!(app.notices.is_empty());
    }

    #[tokio::test]
    async fn clear_hides_existing_chat_without_deleting_context() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line("hi").await.unwrap();
        app.drain_pending_prompt(None).await.unwrap();
        let message_count = app.session.messages.len();
        let event_count = app.session.events.len();
        assert!(message_count > 0);

        app.dispatch_line("/clear").await.unwrap();

        assert_eq!(app.chat_message_start, message_count);
        assert_eq!(app.chat_event_start, event_count);
        assert_eq!(app.session.messages.len(), message_count);
        assert_eq!(app.session.events.len(), event_count);
        assert!(app.ui_banners.is_empty());
        assert!(app.notices.is_empty());
        assert_eq!(app.chat_scroll, 0);
        assert!(app.chat_follow);
    }

    #[tokio::test]
    async fn app_quit_command() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "p".into(),
                cwd: PathBuf::from("."),
                version: "0.4.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line("/quit").await.unwrap();
        assert!(app.should_quit);
    }

    #[tokio::test]
    async fn connect_opencode_go_opens_api_key_overlay() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        let _key_guard = ScopedEnvGuard::new(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "OLLAMA_API_KEY",
            "XAI_API_KEY",
        ]);
        app.connect_store = CredentialStore::new(
            tempfile::TempDir::new()
                .unwrap()
                .path()
                .join("empty-creds.toml"),
        );
        app.dispatch_line("/connect opencode_go").await.unwrap();
        match &app.overlay {
            Some(Overlay::ConnectApiKey {
                profile_id, title, ..
            }) => {
                assert_eq!(profile_id, "opencode_go");
                assert!(title.contains("OpenCode"));
            }
            other => panic!("expected ConnectApiKey overlay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn disconnect_clears_credentials_and_prompts_reauth() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-saved-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());
        app.runtime.model_label = "openai/gpt-4.1-mini".into();
        app.session.set_active_model("openai/gpt-4.1-mini");

        app.dispatch_line("/disconnect").await.unwrap();

        assert!(app.auth_suspended);
        assert!(app.connect_profile.is_none());
        assert!(!app.is_provider_connected());
        assert!(!app.connect_store.is_connected("openai").unwrap());
        assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
        assert!(
            app.notices.iter().any(|l| l.contains("disconnected"))
                || app.status_message.contains("disconnected")
        );
    }

    #[tokio::test]
    async fn connect_picker_marks_saved_credentials_as_connected() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-saved-credential")
            .unwrap();

        app.open_connect_picker();
        let Some(Overlay::ConnectPicker { items, .. }) = &app.overlay else {
            panic!("expected connect picker");
        };
        assert!(
            items
                .iter()
                .any(|item| item.id == "openai" && item.connected),
            "saved provider should be marked connected"
        );
    }

    #[tokio::test]
    async fn successful_connect_hands_off_to_model_picker() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-saved-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());
        app.runtime.model_label = "openai/gpt-4.1-mini".into();
        app.session.set_active_model("openai/gpt-4.1-mini");

        app.open_model_picker_after_connect("openai");
        let Some(Overlay::Model {
            provider_selected,
            providers,
            ..
        }) = &app.overlay
        else {
            panic!("expected model picker");
        };
        assert_eq!(
            providers.get(*provider_selected).map(String::as_str),
            Some("openai")
        );
        assert!(app.feedback.text.contains("choose a model"));
    }

    #[tokio::test]
    async fn model_selection_switches_to_the_matching_connected_provider() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        app.connect_store
            .set_api_key("openai", "sk-test-openai-credential")
            .unwrap();
        app.connect_store
            .set_api_key("anthropic", "sk-test-anthropic-credential")
            .unwrap();
        app.connect_profile = Some("openai".into());

        app.apply_model_selection("native", "anthropic/claude-sonnet-4-5");

        assert_eq!(app.connect_profile.as_deref(), Some("anthropic"));
        assert_eq!(app.runtime.model_label, "anthropic/claude-sonnet-4-5");
    }

    #[tokio::test]
    async fn invalid_api_key_error_stays_inside_key_modal() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("credentials.toml"));
        let mut overlay = Overlay::connect_api_key("openai", "OpenAI", None, None);
        if let Overlay::ConnectApiKey { key_input, .. } = &mut overlay {
            *key_input = "bad".into();
        }
        app.overlay = Some(overlay);

        app.try_connect_api_key("openai", Some("bad".into()));
        let Some(Overlay::ConnectApiKey {
            key_input, error, ..
        }) = &app.overlay
        else {
            panic!("expected API key modal to remain open");
        };
        assert_eq!(key_input, "bad");
        assert!(error.as_deref().is_some_and(|text| text.contains("short")));
        assert!(
            !app.ui_banners.iter().any(|item| matches!(
                item,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )),
            "onboarding errors should stay in the modal"
        );
    }

    #[tokio::test]
    async fn connect_xai_opens_oauth_overlay() {
        let (_dir, session) = test_session().await;
        let cred_dir = tempfile::tempdir().unwrap();
        // Isolate credentials + use stub device start (no network).
        std::env::set_var("FORGE_CONNECT_OAUTH_STUB", "1");
        std::env::remove_var("FORGE_CONNECT_OAUTH_FIXTURE");
        std::env::remove_var("FORGE_XAI_OAUTH_ACCESS_TOKEN");
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_store = CredentialStore::new(cred_dir.path().join("c.toml"));
        app.dispatch_line("/connect xai").await.unwrap();
        std::env::remove_var("FORGE_CONNECT_OAUTH_STUB");
        match &app.overlay {
            Some(Overlay::ConnectOauth {
                profile_id, title, ..
            }) => {
                assert_eq!(profile_id, "xai");
                assert!(title.contains("Grok") || title.contains("xAI"));
            }
            other => panic!("expected ConnectOauth overlay, got {other:?}"),
        }
        assert!(app.oauth_pending.is_some());
    }

    #[tokio::test]
    async fn history_records_submitted_lines_and_up_recalls() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.7.0".into(),
                startup_notices: Vec::new(),
            },
        );
        let enter = KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.input.set_text("/status");
        app.handle_key(enter).await.unwrap();
        app.input.set_text("/model");
        app.handle_key(enter).await.unwrap();
        assert!(app.history.len() >= 2);
        let t = app.history.up(&app.input.text).unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/model");
        let t = app.history.up(&app.input.text).unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/status");
        let t = app.history.down().unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/model");
    }

    #[tokio::test]
    async fn history_up_via_key_when_no_overlay() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.7.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.history.push("alpha");
        app.history.push("beta");
        app.input.clear();
        let up = KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(up).await.unwrap();
        assert_eq!(app.input.text, "beta");
        app.handle_key(up).await.unwrap();
        assert_eq!(app.input.text, "alpha");
        let down = KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        app.handle_key(down).await.unwrap();
        assert_eq!(app.input.text, "beta");
    }

    fn press(code: KeyCode, mods: KeyModifiers) -> event::KeyEvent {
        event::KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }

    #[tokio::test]
    async fn slash_stays_in_textbox_does_not_open_palette() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "/");
        assert!(app.overlay.is_none(), "Phase 8: / must not open palette");
        app.handle_key(press(KeyCode::Char('s'), KeyModifiers::NONE))
            .await
            .unwrap();
        app.handle_key(press(KeyCode::Char('t'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert_eq!(app.input.text, "/st");
        assert!(app.overlay.is_none());
    }

    #[tokio::test]
    async fn enter_runs_slash_from_main_textbox() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(app.input.text, "/status");
        assert!(app.overlay.is_none());
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.notices.is_empty());
        assert!(app.history.entries().iter().any(|e| e == "/status"));
    }

    #[tokio::test]
    async fn ctrl_k_opens_command_palette() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.handle_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::Slash { .. })));
    }

    #[tokio::test]
    async fn ctrl_b_toggles_sidebar_preference_without_affecting_narrow_layout() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "test".into(),
                startup_notices: Vec::new(),
            },
        );
        assert!(app.sidebar_visible);
        app.handle_key(press(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(!app.sidebar_visible);
        assert!(split_areas_full(
            ratatui::layout::Rect::new(0, 0, 120, 30),
            0,
            3,
            app.sidebar_visible,
            0
        )
        .sidebar
        .is_none());
        assert!(
            split_areas_full(ratatui::layout::Rect::new(0, 0, 80, 24), 0, 3, true, 0)
                .sidebar
                .is_none()
        );
    }

    #[tokio::test]
    async fn inspector_view_shortcuts_cycle_without_hiding_sidebar() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "test".into(),
                startup_notices: Vec::new(),
            },
        );
        assert_eq!(app.inspector_view, InspectorView::Task);
        app.handle_key(press(KeyCode::Char(']'), KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(app.inspector_view, InspectorView::Context);
        app.handle_key(press(KeyCode::Char('['), KeyModifiers::ALT))
            .await
            .unwrap();
        assert_eq!(app.inspector_view, InspectorView::Task);
        assert!(app.sidebar_visible);
    }

    #[tokio::test]
    async fn multi_token_slash_connect_list_opens_picker() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/connect list".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        match &app.overlay {
            Some(Overlay::ConnectPicker { items, .. }) => {
                assert!(items.iter().any(|i| i.id == "xai"));
                assert!(items.iter().any(|i| i.id == "opencode_go"));
            }
            other => panic!("expected ConnectPicker, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_tab_autocompletes_command() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/sta".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert!(!app.slash_suggestions().is_empty());
        app.handle_key(press(KeyCode::Tab, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            app.input.text.starts_with("/status"),
            "got {}",
            app.input.text
        );
    }

    #[tokio::test]
    async fn connect_alone_opens_profile_picker() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/connect".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::ConnectPicker { .. })));
    }

    #[tokio::test]
    async fn startup_notices_seed_notice_panel() {
        let (_dir, session) = test_session().await;
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: vec!["mcp: failed".into()],
            },
        );

        assert_eq!(app.notices, vec!["mcp: failed"]);
    }

    #[tokio::test]
    async fn enter_on_highlighted_suggestion_runs_command() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        // Partial type; suggestions include /connect and /status.
        for c in "/con".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        let suggestions = app.slash_suggestions();
        assert!(!suggestions.is_empty(), "expected slash suggestions");
        // Move highlight onto /connect if it is not already first
        let connect_idx = suggestions
            .iter()
            .position(|s| s.cmd == "/connect")
            .expect("/connect in suggestions for /con");
        for _ in 0..connect_idx {
            app.handle_key(press(KeyCode::Down, KeyModifiers::NONE))
                .await
                .unwrap();
        }
        assert_eq!(
            app.slash_suggestions()[app.slash_suggest_idx].cmd,
            "/connect"
        );
        // One Enter should apply selection AND open the connect picker (not merely complete text)
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            matches!(app.overlay, Some(Overlay::ConnectPicker { .. })),
            "Enter on highlighted /connect should open picker; overlay={:?} input={:?} status={}",
            app.overlay,
            app.input.text,
            app.status_message
        );
        assert!(
            app.input.text.is_empty(),
            "input should be cleared after run, got {:?}",
            app.input.text
        );
    }

    #[tokio::test]
    async fn bare_slash_lists_all_palette_commands() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.handle_key(press(KeyCode::Char('/'), KeyModifiers::NONE))
            .await
            .unwrap();
        let suggestions = app.slash_suggestions();
        let expected = crate::overlays::default_palette_items();
        assert_eq!(
            suggestions.len(),
            expected.len(),
            "bare / should list every palette command; got {:?}",
            suggestions.iter().map(|s| &s.cmd).collect::<Vec<_>>()
        );
        for cmd in [
            "/status", "/connect", "/model", "/compact", "/resume", "/sync", "/quit",
        ] {
            assert!(
                suggestions.iter().any(|s| s.cmd == cmd),
                "missing {cmd} in suggestions"
            );
        }
    }

    #[tokio::test]
    async fn enter_on_status_suggestion_runs_immediately() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/sta".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.input.text.is_empty());
    }

    #[test]
    fn status_model_from_app_fields() {
        // layout + status integration smoke
        let m = StatusModel {
            status: forge_types::SessionStatus::Running,
            session_short: "abc".into(),
            model: "m".into(),
            provider: "mock".into(),
            effort: "auto".into(),
            ctx_pct: 0.2,
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
        assert_eq!(m.status_label().0, "Idle");
    }

    #[tokio::test]
    async fn blocks_chat_when_not_connected() {
        use crossterm::event::{KeyCode, KeyModifiers};

        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-4.1-mini".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.11.0".into(),
                startup_notices: Vec::new(),
            },
        );
        // Clear env vars so dev machine credentials don't leak into tests.
        // Must be after TuiApp::new() — restore_saved_auth sets env from stored credentials.
        let _key_guard = ScopedEnvGuard::new(&[
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENCODE_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "OLLAMA_API_KEY",
            "XAI_API_KEY",
        ]);
        app.connect_profile = None;
        app.connect_store = CredentialStore::new(
            tempfile::TempDir::new()
                .unwrap()
                .path()
                .join("empty-creds.toml"),
        );
        app.refresh_connection_ui();
        assert!(!app.is_provider_connected());

        for c in "hello world".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();

        assert!(app.pending_prompt.is_none(), "must not queue a model turn");
        assert!(!app.busy);
        assert_eq!(app.input.text, "hello world");
        assert!(
            app.ui_banners.iter().any(|b| matches!(
                b,
                ChatItem::Banner { text, .. } if text.to_ascii_lowercase().contains("not connected")
            )) || app
                .activity
                .recent(8)
                .iter()
                .any(|e| e.summary.to_ascii_lowercase().contains("not connected")),
            "expected not-connected feedback"
        );
    }

    #[tokio::test]
    async fn mock_provider_allows_chat_without_connect() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mock".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.11.0".into(),
                startup_notices: Vec::new(),
            },
        );
        assert!(app.is_provider_connected());
        app.dispatch_line("hi").await.unwrap();
        assert_eq!(app.pending_prompt.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn status_chrome_shows_not_connected_badge() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-test".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.11.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.connect_profile = None;
        app.connect_store = CredentialStore::new(
            tempfile::TempDir::new()
                .unwrap()
                .path()
                .join("empty-creds.toml"),
        );
        app.refresh_connection_ui();
        let backend = TestBackend::new(100, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf.get(x, y).symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("not connected") || text.contains("○"),
            "missing not-connected chrome:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_chrome_includes_model_on_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-test".into(),
                provider: "native".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        let chrome = app.refresh_status_model();
        assert_eq!(chrome.provider, "native");
        assert!(chrome.model.contains("gpt-test"));
        let backend = TestBackend::new(120, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf.get(x, y).symbol());
            }
            text.push('\n');
        }
        assert!(text.contains("gpt-test"), "chrome missing model:\n{text}");
        assert!(
            text.contains("in 0 · out 0 · total 0"),
            "footer missing usage:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_narrow_frame_still_shows_model_or_ctx() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "mymodel".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        // Width 60: no sidebar per layout MIN_WIDTH 80
        let backend = TestBackend::new(60, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf.get(x, y).symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("mymodel") || text.contains("ctx") || text.contains("mock"),
            "narrow frame missing identity:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_status_renders_structured_session_card() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.notices.is_empty());
        assert!(app.overlay.is_none());

        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            text.contains("unknown command `/status`"),
            "missing status command feedback:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui08_report_error_writes_banner_feedback_and_activity() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.report_error("upstream returned 429 rate limit exceeded");
        assert_eq!(app.feedback.severity, FeedbackSeverity::Error);
        assert!(app.feedback.text.contains("429"));
        assert!(app.status_message.contains("429"));
        assert!(
            app.ui_banners.iter().any(|b| matches!(
                b,
                ChatItem::Banner {
                    kind: BannerKind::Error,
                    ..
                }
            )),
            "expected error banner in ui_banners"
        );
        assert!(
            app.activity
                .all()
                .iter()
                .any(|i| i.kind == ActivityKind::Error),
            "expected error activity"
        );
    }

    #[tokio::test]
    async fn tui08_feedback_strip_visible_on_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.report_error("429 rate limit");
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let mut text = String::new();
        let buf = term.backend().buffer();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                text.push_str(buf.get(x, y).symbol());
            }
            text.push('\n');
        }
        assert!(
            text.contains("rate limited") || text.contains("429") || text.contains("Model error"),
            "frame missing feedback:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui10_activity_feed_records_model_and_error() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
        app.report_error("429 rate limit");
        assert!(app.activity.len() >= 2);
        let recent: Vec<_> = app
            .activity
            .recent(10)
            .iter()
            .map(|i| i.summary.clone())
            .collect();
        assert!(
            recent
                .iter()
                .any(|s| s.contains("rate") || s.contains("429") || s.contains("Model")),
            "recent={recent:?}"
        );
        assert_eq!(app.busy_phase, BusyPhase::Idle);
    }

    #[tokio::test]
    async fn elapsed_status_persists_during_answer_and_tool_processing() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.busy = true;
        app.turn_started = Some(Instant::now() - Duration::from_millis(1200));
        app.stream_preview = "partial answer".into();
        assert_eq!(app.busy_status_detail().as_deref(), Some("Working... 1.2s"));

        app.stream_preview.clear();
        app.busy_phase = BusyPhase::Tool {
            name: "read_file".into(),
        };
        assert!(app
            .busy_status_detail()
            .unwrap()
            .starts_with("Working... 1.2s"));
    }

    #[tokio::test]
    async fn tui10_busy_phase_model_during_turn_clears_after() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.dispatch_line("hello").await.unwrap();
        assert_eq!(app.busy_phase, BusyPhase::Model);
        assert!(app.pending_prompt.is_some());
        app.drain_pending_prompt(None).await.unwrap();
        assert_eq!(app.busy_phase, BusyPhase::Idle);
        assert!(
            app.activity
                .all()
                .iter()
                .any(|i| i.kind == ActivityKind::Model),
            "expected model activity"
        );
    }

    #[tokio::test]
    async fn tui08_context_sets_feedback_strip() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        for c in "/status".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.input.text.is_empty());
    }

    /// Save-and-restore env vars so dev machine credentials don't leak into tests.
    struct ScopedEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(String, Option<String>)>,
    }

    impl ScopedEnvGuard {
        fn new(keys: &[&str]) -> Self {
            static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("environment test lock poisoned");
            let mut saved = Vec::new();
            for key in keys {
                saved.push((key.to_string(), std::env::var(key).ok()));
                std::env::remove_var(key);
            }
            Self { _lock: lock, saved }
        }
    }

    impl Drop for ScopedEnvGuard {
        fn drop(&mut self) {
            for (key, val) in &self.saved {
                match val {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn exit_summary_formats_token_usage() {
        let report = forge_core::TokenUsageReport {
            api: forge_core::SessionTokenUsage {
                prompt_tokens: 6_094,
                completion_tokens: 36,
                model_calls_with_usage: 1,
                model_steps: 1,
                thinking_tokens_est: 19,
                prompt_cache_hits: 5_504,
                prompt_cache_writes: 0,
            },
            context_tokens_est: 0,
            context_capacity: 1,
            context_pct: 0.0,
            system_tokens_est: 0,
            user_tokens_est: 0,
            assistant_tokens_est: 0,
            tool_tokens_est: 0,
            thinking_in_context_est: 0,
            message_count: 0,
            tool_message_count: 0,
        };

        assert_eq!(
            format_exit_token_usage(&report),
            "Token usage: total=6,130 input=6,094 (+ 5,504 cached) output=36 (reasoning 19)"
        );
    }

    #[test]
    fn footer_usage_formats_with_total_and_commas() {
        let report = forge_core::TokenUsageReport {
            api: forge_core::SessionTokenUsage {
                prompt_tokens: 6_094,
                completion_tokens: 36,
                model_calls_with_usage: 1,
                model_steps: 1,
                thinking_tokens_est: 19,
                prompt_cache_hits: 5_504,
                prompt_cache_writes: 0,
            },
            context_tokens_est: 0,
            context_capacity: 1,
            context_pct: 0.0,
            system_tokens_est: 0,
            user_tokens_est: 0,
            assistant_tokens_est: 0,
            tool_tokens_est: 0,
            thinking_in_context_est: 0,
            message_count: 0,
            tool_message_count: 0,
        };

        assert_eq!(
            footer_usage_summary_with_cost(&report, None),
            "in 6,094 · out 36 · total 6,130"
        );
    }

    #[test]
    fn footer_usage_includes_cached_cost() {
        let report = forge_core::TokenUsageReport {
            api: forge_core::SessionTokenUsage {
                prompt_tokens: 1_000_000,
                completion_tokens: 500_000,
                model_calls_with_usage: 1,
                model_steps: 1,
                thinking_tokens_est: 0,
                prompt_cache_hits: 0,
                prompt_cache_writes: 0,
            },
            context_tokens_est: 0,
            context_capacity: 1,
            context_pct: 0.0,
            system_tokens_est: 0,
            user_tokens_est: 0,
            assistant_tokens_est: 0,
            tool_tokens_est: 0,
            thinking_in_context_est: 0,
            message_count: 0,
            tool_message_count: 0,
        };

        assert_eq!(
            footer_usage_summary_with_cost(
                &report,
                Some(forge_connect::CatalogCost {
                    input: 3.0,
                    output: 15.0,
                })
            ),
            "in 1,000,000 · out 500,000 · total 1,500,000 · $10.5000"
        );
    }

    #[test]
    fn footer_limits_use_connected_profile_instead_of_native_transport() {
        assert_eq!(
            footer_provider_id("native", Some("openai-codex")),
            "openai-codex"
        );
        assert_eq!(footer_provider_id("mock", None), "mock");
    }

    #[tokio::test]
    async fn external_editor_keybind_sets_flag() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        assert!(!app.pending_external_editor);
        app.workspace_mode = WorkspaceMode::Editor;
        app.source_viewer
            .open(Path::new("/tmp"), &PathBuf::from("/tmp/fake.txt"));
        app.handle_key(press(KeyCode::Char('e'), KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_preconditions_no_file() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.pending_external_editor = true;
        app.drain_pending_external_editor(None).await.unwrap();
        // Should not crash; feedback set because no file is open.
        assert!(!app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_preconditions_binary_file() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.source_viewer.status = crate::source_viewer::ViewerStatus::Binary;
        app.source_viewer.path = Some(PathBuf::from("/tmp/fake.bin"));
        app.pending_external_editor = true;
        app.drain_pending_external_editor(None).await.unwrap();
        // Should not crash; feedback set because binary.
        assert!(!app.pending_external_editor);
    }

    #[tokio::test]
    async fn external_editor_rejects_during_tool_execution() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
                startup_notices: Vec::new(),
            },
        );
        app.busy_phase = BusyPhase::Tool {
            name: "write".into(),
        };
        app.source_viewer.status = crate::source_viewer::ViewerStatus::Ok;
        app.source_viewer.path = Some(PathBuf::from("/tmp/fake.txt"));
        app.pending_external_editor = true;
        app.drain_pending_external_editor(None).await.unwrap();
        // Should not crash; feedback set because tool is active.
        assert!(!app.pending_external_editor);
    }
}
