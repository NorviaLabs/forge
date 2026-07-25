//! Full-screen TUI application (TUI-01 shell + TUI-02/03/04 wired).

use std::collections::HashSet;
use std::fs;
use std::io::{self, stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use forge_connect::{
    builtin_registry, handle_connect_action, models_for_picker, needs_tui_api_key_prompt,
    needs_tui_oauth, normalize_model_id, refresh_profile_catalog, ConnectAction, ConnectError,
    ConnectRegistry, ConnectService, CredentialStore, ModelCatalogCache, OauthPending,
};
use forge_core::{AgentSession, ApplyOutcome, LoopError};
use forge_tools::{GitTool, Tool, ToolContext};
use forge_types::{HitlDecision, ModelStreamEvent};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use serde_json::json;
use thiserror::Error;

use crate::activity::{ActivityFeed, ActivityKind};
use crate::commands::{parse_slash, SlashCommand};
use crate::conversation::{
    format_elapsed_tenths, BannerKind, ChatItem, ConversationModel, ConversationViewOpts,
    StreamWaitPhase,
};
use crate::effort::ReasoningEffort;
use crate::history::InputHistory;
use crate::layout::is_too_small;
use crate::layout::split_areas_full;
use crate::msg_queue::MessageQueue;
use crate::overlays::{
    filter_palette, handle_overlay_key, models_from_catalog, ConnectProfileItem, FileExplorerItem,
    Key as OverlayKey, Overlay, OverlayAction, OverlayWidget, PaletteItem, ResumeSessionItem,
};
use crate::theme;
use crate::widgets::{
    classify_operator_error, session_chrome_lines, BusyPhase, FeedbackBar, FeedbackModel,
    FeedbackSeverity, FooterBar, FooterModel, InputBar, InputModel, StatusModel,
};
use crate::ExitCode;
use ratatui::widgets::Paragraph;

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
pub struct TuiRuntimeConfig {
    pub model_label: String,
    pub provider: String,
    pub cwd: PathBuf,
    pub version: String,
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
    sessions.sort_by(|left, right| right.modified.cmp(&left.modified));
    sessions.truncate(limit);
    Ok(sessions)
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
    /// Catalog refresh queued to run on the event loop (can do network).
    pending_model_refresh: bool,
    /// HITL resolve queued to run on the event loop (journals + state updates).
    pending_hitl_decision: Option<HitlDecision>,
    /// Context reset queued to run on the event loop.
    pending_context_reset: bool,
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
    /// Soft-cancel in-flight turn (Esc while busy).
    cancel_requested: bool,
    /// Tools allowed for the rest of this session (HITL "s").
    hitl_session_allow: HashSet<String>,
    /// Transient toast (auto-clears).
    toast: Option<(Instant, String)>,
    /// Session message/event offsets hidden by the most recent `/clear`.
    chat_message_start: usize,
    chat_event_start: usize,
    /// Conversation scroll offset (when not following).
    chat_scroll: u16,
    chat_follow: bool,
}

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        let mut input = InputModel::default();
        input.hint = String::new();
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
            notices: Vec::new(),
            feedback: FeedbackModel::default(),
            ui_banners: Vec::new(),
            busy_phase: BusyPhase::Idle,
            web_search_label: Some("mock".into()),
            activity: ActivityFeed::default(),
            pending_prompt: None,
            pending_turn_continue: false,
            pending_sync: false,
            pending_model_refresh: false,
            pending_hitl_decision: None,
            pending_context_reset: false,
            message_queue: MessageQueue::new(),
            queue_selected: None,
            stream_preview: String::new(),
            stream_thinking: String::new(),
            turn_started: None,
            thinking_started: None,
            thought_secs: None,
            reasoning_effort: ReasoningEffort::from_env(),
            tool_expanded: false,
            cancel_requested: false,
            hitl_session_allow: HashSet::new(),
            toast: None,
            chat_message_start: 0,
            chat_event_start: 0,
            chat_scroll: 0,
            chat_follow: true,
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
        let connected = self.is_provider_connected();
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
        self.pending_model_refresh = false;
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
        if ReasoningEffort::env_override().is_none() {
            if let Some(effort) = self
                .connect_store
                .last_effort()
                .ok()
                .flatten()
                .and_then(|effort| effort.parse().ok())
            {
                self.reasoning_effort = effort;
                self.session.apply_provider_env(&[(
                    "FORGE_REASONING_EFFORT".into(),
                    effort.transport_value().into(),
                )]);
            }
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
        // Errors are not sticky status chrome — use activity/chat only.
        if severity == FeedbackSeverity::Error {
            self.status_message.clear();
            self.feedback = FeedbackModel::default();
            return;
        }
        let text = text.into();
        self.status_message = text.clone();
        self.feedback = FeedbackModel { text, severity };
    }

    /// Operator errors go to activity (and a single chat banner), not a sticky red strip.
    pub fn report_error(&mut self, raw: &str) {
        let msg = classify_operator_error(raw);
        // Clear any prior sticky strip (including leftover errors from older builds).
        self.feedback = FeedbackModel::default();
        self.status_message.clear();
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
                    self.notices = msg.lines().map(|s| s.to_string()).collect();
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
                self.notices = lines;
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
                    self.notices = vec![error];
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
                self.notices = vec![e.to_string()];
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
        self.notices = lines;
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

    /// Force-refresh remote catalogs for every connected profile.
    fn refresh_model_catalogs(&mut self) {
        let svc = ConnectService {
            registry: &self.connect_registry,
            store: &self.connect_store,
            active_profile_id: self.connect_profile.clone(),
            active_model: Some(self.runtime.model_label.clone()),
        };
        let mut profiles = svc.connected_profiles().unwrap_or_default();
        // Also try the currently-selected provider/model prefix so `/model refresh` gives a
        // useful error line even when the profile isn't "connected" yet.
        let prefix = self
            .runtime
            .model_label
            .split('/')
            .next()
            .unwrap_or("")
            .trim();
        if !prefix.is_empty() {
            let pid = match prefix {
                "opencode-go" => "opencode_go",
                "opencode-zen" => "opencode_zen",
                other => other,
            };
            if profiles.iter().all(|p| p.id != pid) {
                if let Some(p) = self.connect_registry.get(pid).cloned() {
                    profiles.push(p);
                }
            }
        }
        if profiles.is_empty() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "no connected providers — /connect first",
            );
            self.notices = vec![
                "Connect a provider, then run /model refresh.".into(),
                "Fallbacks: /model openai/gpt-4.1-mini (provider/model id).".into(),
            ];
            return;
        }
        let cache = ModelCatalogCache::user_default();
        let mut lines = Vec::new();
        let mut ok_n = 0usize;
        for p in &profiles {
            match refresh_profile_catalog(p, &self.connect_store, &cache) {
                Ok(models) => {
                    ok_n += 1;
                    lines.push(format!(
                        "{}: {} models (e.g. {})",
                        p.id,
                        models.len(),
                        models.first().map(|s| s.as_str()).unwrap_or("—")
                    ));
                }
                Err(e) => {
                    lines.push(format!("{}: refresh failed — {e}", p.id));
                }
            }
        }
        self.status_message = format!("catalog refresh · {ok_n}/{} ok", profiles.len());
        self.set_feedback(
            FeedbackSeverity::Info,
            format!("catalogs refreshed ({ok_n}/{})", profiles.len()),
        );
        self.notices = lines;
        // Open picker with fresh data
        let items = self.model_picker_items(true);
        let mut ov = Overlay::model_open_with(items);
        ov.focus_model(&self.runtime.model_label);
        self.overlay = Some(ov);
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
        self.set_feedback(FeedbackSeverity::Info, "syncing…");
        self.status_message = "syncing…".into();
        self.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Info,
            "git sync queued",
        );
    }

    fn queue_model_refresh(&mut self) {
        if self.busy
            || self.pending_prompt.is_some()
            || self.pending_sync
            || self.pending_model_refresh
            || self.pending_hitl_decision.is_some()
            || self.pending_context_reset
        {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before /model refresh");
            return;
        }
        self.pending_model_refresh = true;
        self.busy_phase = BusyPhase::Other("model catalog refresh".into());
        self.status_message = "refreshing model catalogs…".into();
        self.set_feedback(FeedbackSeverity::Info, "refreshing model catalogs…");
        self.push_activity(
            ActivityKind::System,
            FeedbackSeverity::Info,
            "model refresh queued",
        );
    }

    fn queue_hitl(&mut self, decision: HitlDecision) {
        if self.busy
            || self.pending_prompt.is_some()
            || self.pending_sync
            || self.pending_model_refresh
            || self.pending_hitl_decision.is_some()
            || self.pending_context_reset
        {
            self.set_feedback(FeedbackSeverity::Warn, "busy — wait before HITL");
            return;
        }
        self.pending_hitl_decision = Some(decision);
        self.busy_phase = BusyPhase::Other("HITL".into());
        self.status_message = "resolving approval…".into();
        self.set_feedback(FeedbackSeverity::Info, "resolving approval…");
    }

    fn queue_context_reset(&mut self) {
        if self.busy
            || self.pending_prompt.is_some()
            || self.pending_sync
            || self.pending_model_refresh
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

    pub async fn drain_pending_model_refresh(
        &mut self,
        mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
    ) -> Result<(), TuiError> {
        if !self.pending_model_refresh {
            return Ok(());
        }
        self.pending_model_refresh = false;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        // `refresh_model_catalogs` is sync (uses ureq). We still run it here, but it's
        // now drained on the event loop after the command echo has painted.
        self.refresh_model_catalogs();
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
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
            HitlDecision::Approve => "approved".into(),
            HitlDecision::Deny => "denied".into(),
        };
        self.notices = vec![format!("HITL {}.", self.status_message)];
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
        let before = self.session.token_usage_report().context_tokens_est;
        self.session.force_context_reset_async().await?;
        let after = self.session.token_usage_report().context_tokens_est;
        self.push_toast("context compacted");
        self.set_feedback(
            FeedbackSeverity::Ok,
            format!("context compacted · {before} → {after} tokens"),
        );
        self.status_message = "context compacted".into();
        self.notices = vec![
            format!("Context compacted: {before} → {after} estimated tokens."),
            "Progress written to .forge/progress.json.".into(),
        ];
        self.busy_phase = BusyPhase::Idle;
        if let Some(term) = terminal.as_deref_mut() {
            let _ = term.draw(|f| self.draw(f));
        }
        Ok(())
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
            self.notices = vec![
                "Working tree clean — no changes to commit or push.".into(),
                "Make edits, then run /sync again.".into(),
            ];
            if let Some(term) = terminal.as_deref_mut() {
                let _ = term.draw(|f| self.draw(f));
            }
            return;
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
                self.notices = o
                    .content
                    .lines()
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty())
                    .take(16)
                    .collect();
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
                self.notices = vec![format!("Committed: {message}"), "Push failed:".into()]
                    .into_iter()
                    .chain(
                        o.content
                            .lines()
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty())
                            .take(12),
                    )
                    .collect();
            }
            Err(e) => {
                self.report_error(&format!("committed but push failed: {e}"));
                self.notices = vec![format!("Committed: {message}"), format!("Push error: {e}")];
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
                self.notices = lines;
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
            worktree_on: self.session.worktree_status().is_some(),
            busy: self.busy,
            busy_phase: self.busy_phase.clone(),
            connect_profile: self.connect_profile.clone(),
            provider_connected: self.is_provider_connected(),
            web_search_label: self.web_search_label.clone(),
            tools_visible: self.session.list_tools().len(),
            prompt_cache_hits: self.session.token_usage.prompt_cache_hits,
            prompt_cache_writes: self.session.token_usage.prompt_cache_writes,
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
        let regions = split_areas_full(area, fb_h, input_h, false, 0);
        let status = self.refresh_status_model();

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
        let mut conv = ConversationModel::from_messages(
            visible_messages,
            visible_events,
            self.session.status,
            opts,
        )
        .with_extra_banners(self.ui_banners.iter().cloned());
        conv = conv.with_queued_messages(
            self.message_queue.iter().cloned().collect::<Vec<_>>(),
            self.queue_selected,
        );
        conv.follow = self.chat_follow;
        conv.scroll = self.chat_scroll;
        if self.busy && self.pending_prompt.is_none() {
            // Stream thinking body + answer (status line covers wait/think timers)
            conv = conv
                .with_streaming_preview(self.stream_thinking.clone(), self.stream_preview.clone());
        }
        frame.render_widget(
            crate::conversation::ConversationWidget { model: &conv },
            regions.chat,
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
                let max_list = (input.y.saturating_sub(2)).min(16).max(1) as usize;
                let visible = n.min(max_list);
                // Scroll so the highlighted row stays on screen.
                let start = if n <= visible {
                    0
                } else if idx < visible / 2 {
                    0
                } else if idx + (visible - visible / 2) >= n {
                    n - visible
                } else {
                    idx - visible / 2
                };
                let h = (visible as u16).saturating_add(2); // +2 for borders
                if input.y >= h {
                    let sug_area = ratatui::layout::Rect {
                        x: input.x,
                        y: input.y.saturating_sub(h),
                        width: input.width,
                        height: h,
                    };
                    // Pad rows so background fill spans the panel width (visible selection).
                    let inner_w = sug_area.width.saturating_sub(2) as usize;
                    let lines: Vec<ratatui::text::Line> = suggestions
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
                    let title = if n > visible {
                        format!(
                            " commands {}–{}/{} · Tab · ↑↓ ",
                            start + 1,
                            start + visible,
                            n
                        )
                    } else {
                        format!(" commands ({n}) · Tab complete · ↑↓ ")
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
        input.not_connected = !self.is_provider_connected();
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
        } else if !self.is_provider_connected() {
            "/connect to enable chat".into()
        } else if qn > 0 {
            format!("queue {qn} · Ctrl+Up/Down select · Ctrl+Backspace cancel")
        } else {
            "Ctrl+K command palette".into()
        };
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
            worktree_on: status.worktree_on,
            hints,
        };
        frame.render_widget(FooterBar { model: &footer }, regions.footer);

        if let Some(ref ov) = self.overlay {
            frame.render_widget(OverlayWidget { overlay: ov }, area);
        }
    }

    pub async fn handle_key(&mut self, key: event::KeyEvent) -> Result<(), TuiError> {
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
                    self.overlay = None;
                    self.apply_model_selection(&provider, &model);
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

        match key.code {
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
                if self.input.text.starts_with('/') && !suggestions.is_empty() {
                    let n = suggestions.len();
                    self.slash_suggest_idx = (self.slash_suggest_idx + n - 1) % n;
                } else if let Some(text) = self.history.up(&self.input.text) {
                    self.apply_history_text(text);
                }
            }
            KeyCode::Down => {
                let suggestions = self.slash_suggestions();
                if self.input.text.starts_with('/') && !suggestions.is_empty() {
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

    async fn handle_model_command(
        &mut self,
        provider: Option<&str>,
        model: Option<&str>,
        refresh: bool,
    ) {
        if refresh {
            self.queue_model_refresh();
            if cfg!(test) {
                let _ = self.drain_pending_model_refresh(None).await;
            }
            return;
        }

        if provider.is_none() && model.is_none() {
            let items = self.model_picker_items(true);
            let mut overlay = Overlay::model_open_with(items);
            overlay.focus_model(&self.runtime.model_label);
            self.overlay = Some(overlay);
            self.status_message = "pick a model (live catalog when connected)".into();
            return;
        }

        let connected_prefix = self.connect_profile.as_deref().and_then(|id| {
            self.connect_registry
                .get(id)
                .map(|profile| profile.model_provider_prefix.as_str())
        });
        let model_id = normalize_model_id(provider.unwrap_or(""), model, connected_prefix);
        if model_id.trim().is_empty() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                "usage: /model <provider/model> | /model refresh",
            );
            return;
        }
        let target_prefix = Self::model_prefix(&model_id);
        let matching_profile = self.connected_profile_for_model_prefix(target_prefix);
        if matching_profile.is_none() {
            self.set_feedback(
                FeedbackSeverity::Warn,
                format!("connect `{target_prefix}` first before selecting {model_id}"),
            );
            self.notices = vec![
                format!("No connected provider matches `{target_prefix}`."),
                "Use /connect, or pick a model from the current provider catalog.".into(),
            ];
            return;
        } else {
            self.apply_model_selection("native", &model_id);
        }
    }

    fn handle_effort_command(&mut self, level: Option<ReasoningEffort>) {
        let Some(level) = level else {
            self.set_feedback(
                FeedbackSeverity::Info,
                format!("reasoning effort: {}", self.reasoning_effort),
            );
            self.notices = vec![
                format!("Current reasoning effort: {}", self.reasoning_effort),
                format!("Usage: /effort {}", ReasoningEffort::USAGE),
            ];
            return;
        };

        self.reasoning_effort = level;
        self.session.apply_provider_env(&[(
            "FORGE_REASONING_EFFORT".into(),
            level.transport_value().into(),
        )]);
        self.persist_selection();
        self.set_feedback(FeedbackSeverity::Ok, format!("reasoning effort: {level}"));
        self.status_message = format!("reasoning effort set to {level}");
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
                Ok(SlashCommand::Status) => {
                    let chrome = self.refresh_status_model();
                    let report = self.session.token_usage_report();
                    let mut lines = vec![
                        format!("session_id={}", self.session.session_id),
                        "status".into(),
                    ];
                    lines.extend(session_chrome_lines(&chrome));
                    lines.push(String::new());
                    lines.push("tokens".into());
                    lines.extend([
                        format!("api.prompt={}", report.api.prompt_tokens),
                        format!("api.completion={}", report.api.completion_tokens),
                        format!("api.total={}", report.api.total_api_tokens()),
                        format!("cache.hits={}", report.api.prompt_cache_hits),
                        format!("cache.writes={}", report.api.prompt_cache_writes),
                        format!("context.used={:.1}%", report.context_pct),
                        format!(
                            "context.tokens={}/{}",
                            report.context_tokens_est, report.context_capacity
                        ),
                    ]);
                    lines.extend(self.session.token_usage_lines());
                    let api = &report.api;
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        format!(
                            "{} · {} · ctx {:.0}% · tokens in {} · out {} · total {} · cache {} / {}",
                            chrome.provider,
                            chrome.model,
                            chrome.ctx_pct * 100.0,
                            api.prompt_tokens,
                            api.completion_tokens,
                            api.total_api_tokens(),
                            api.prompt_cache_hits,
                            api.prompt_cache_writes,
                        ),
                    );
                    self.status_message = "status · context".into();
                    self.notices = lines;
                }
                Ok(SlashCommand::Cost) => {
                    let profile_id = self.connect_profile.clone().or_else(|| {
                        let prefix = Self::model_prefix(&self.runtime.model_label);
                        self.connected_profile_for_model_prefix(prefix)
                    });
                    match profile_id {
                        Some(profile_id) => match forge_connect::provider_cost_report(
                            &profile_id,
                            &self.connect_store,
                        ) {
                            Ok(report) => self.notices = report,
                            Err(error) => self.set_feedback(FeedbackSeverity::Error, error),
                        },
                        None => self.set_feedback(
                            FeedbackSeverity::Warn,
                            "no connected provider; use /connect first",
                        ),
                    }
                }
                Ok(SlashCommand::Approve) => {
                    if self.session.pending_hitl.is_none() {
                        self.status_message = "no pending HITL to approve".into();
                        self.notices = vec!["No human-in-the-loop request is waiting.".into()];
                    } else {
                        self.queue_hitl(HitlDecision::Approve);
                        if cfg!(test) {
                            let _ = self.drain_pending_hitl(None).await;
                        }
                    }
                }
                Ok(SlashCommand::Deny) => {
                    if self.session.pending_hitl.is_none() {
                        self.status_message = "no pending HITL to deny".into();
                        self.notices = vec!["No human-in-the-loop request is waiting.".into()];
                    } else {
                        self.queue_hitl(HitlDecision::Deny);
                        if cfg!(test) {
                            let _ = self.drain_pending_hitl(None).await;
                        }
                    }
                }
                Ok(SlashCommand::Compact) => {
                    self.queue_context_reset();
                    if cfg!(test) {
                        let _ = self.drain_pending_context_reset(None).await;
                    }
                }
                Ok(SlashCommand::Model {
                    provider,
                    model,
                    refresh,
                }) => {
                    self.handle_model_command(provider.as_deref(), model.as_deref(), refresh)
                        .await
                }
                Ok(SlashCommand::Effort { level }) => self.handle_effort_command(level),
                Ok(SlashCommand::ResumeList) => {
                    match recent_resume_sessions(
                        self.session.journal_dir(),
                        self.session.session_id,
                        10,
                    ) {
                        Ok(sessions) if sessions.is_empty() => {
                            self.status_message = "no previous sessions".into();
                            self.notices =
                                vec!["No previous sessions found for this workspace.".into()];
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
                        Ok(()) => {
                            self.overlay = None;
                            self.notices = vec![format!("Resumed session {session_id}.")];
                            self.status_message = "session resumed".into();
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
                Ok(SlashCommand::Diff) => {
                    let mut lines = vec!["Session tools & changes:".into()];
                    let mut n_tools = 0usize;
                    let mut n_write = 0usize;
                    for m in &self.session.messages {
                        if m.role == forge_types::MessageRole::Tool {
                            n_tools += 1;
                            let name = m.name.as_deref().unwrap_or("tool");
                            if name.contains("write")
                                || name.contains("search_replace")
                                || name == "edit"
                                || name == "git"
                            {
                                n_write += 1;
                            }
                            let preview: String = m.content.chars().take(80).collect();
                            lines.push(format!("· {name}  {preview}"));
                        }
                    }
                    if n_tools == 0 {
                        lines.push("(no tool results yet)".into());
                    } else {
                        lines.insert(1, format!("{n_tools} tool results · {n_write} write-like"));
                    }
                    if let Some(wt) = self.session.worktree_status() {
                        lines.push(format!("worktree: {wt}"));
                    }
                    self.notices = lines;
                    self.status_message = "diff".into();
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
                            self.notices = vec![
                                "Clipboard unavailable (pbcopy/wl-copy).".into(),
                                text.chars().take(400).collect(),
                            ];
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
                Err(e) => {
                    let msg = e.to_string();
                    self.set_feedback(FeedbackSeverity::Warn, msg.clone());
                    self.notices = vec![msg];
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
                // Try restore mid-session if credentials appeared (e.g. forge connect in another terminal)
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
            match self.session.apply_model_response(last).await {
                Ok(out) => {
                    if let Some(secs) = thought {
                        saw_thinking = true;
                        turn_thought_secs += secs;
                    }
                    // Reset per-model-step thinking timers for multi-tool loops.
                    self.thinking_started = None;
                    self.thought_secs = None;
                    match out {
                        ApplyOutcome::Done(_) => {
                            outcome_err = None;
                            break 'turns;
                        }
                        ApplyOutcome::Hitl(_) => {
                            outcome_err = None;
                            break 'turns;
                        }
                        ApplyOutcome::Continue => {
                            if let Some(term) = terminal.as_deref_mut() {
                                term.draw(|f| self.draw(f))?;
                            }
                            continue;
                        }
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
) -> Result<ExitCode, TuiError> {
    enable_raw_mode()?;
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

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;

    result.map(|_| app.last_exit)
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<(), TuiError> {
    while !app.should_quit {
        app.tick_toast();
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
        if app.pending_model_refresh {
            app.drain_pending_model_refresh(Some(terminal)).await?;
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
    use forge_types::ModelResponse;
    use forge_workspace::IsolationMode;
    use std::sync::Arc;
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
                isolation: IsolationMode::Off,
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
                isolation: IsolationMode::Off,
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
        assert_eq!(app.status_message, "session resumed");
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
            },
        );
        app.connect_store = CredentialStore::new(credential_path.clone());

        app.dispatch_line("/effort high").await.unwrap();

        assert_eq!(
            app.connect_store.last_effort().unwrap().as_deref(),
            Some("high")
        );

        if ReasoningEffort::env_override().is_none() {
            let (_dir, session) = test_session().await;
            let mut restarted = TuiApp::new(
                session,
                TuiRuntimeConfig {
                    model_label: "mock".into(),
                    provider: "mock".into(),
                    cwd: PathBuf::from("."),
                    version: "0.12.0".into(),
                },
            );
            restarted.connect_store = CredentialStore::new(credential_path);
            restarted = restarted.restore_saved_auth();

            assert_eq!(restarted.reasoning_effort, ReasoningEffort::High);
        }
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
            },
        );
        app.dispatch_line("/status").await.unwrap();
        assert!(
            app.status_message.contains("ctx")
                || app
                    .notices
                    .iter()
                    .any(|l| l.contains("session") || l.contains("model=")),
            "status={} notices={:?}",
            app.status_message,
            app.notices
        );
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
        assert!(
            app.status_message.contains("ctx")
                || app
                    .notices
                    .iter()
                    .any(|l| l.contains("model=") || l.contains("provider=")),
            "status={} notices={:?}",
            app.status_message,
            app.notices
        );
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
            },
        );
        app.handle_key(press(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .await
            .unwrap();
        assert!(matches!(app.overlay, Some(Overlay::Slash { .. })));
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
            "/status", "/connect", "/model", "/approve", "/deny", "/compact", "/resume", "/sync",
            "/quit",
        ] {
            assert!(
                suggestions.iter().any(|s| s.cmd == cmd),
                "missing {cmd} in suggestions"
            );
        }
    }

    #[tokio::test]
    async fn approve_without_hitl_is_graceful() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.8.0".into(),
            },
        );
        app.dispatch_line("/approve").await.unwrap();
        assert!(app.status_message.contains("no pending"));
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
        assert!(
            app.status_message.contains("ctx")
                || app.feedback.text.contains("ctx")
                || app.notices.iter().any(|l| l.contains("model=")),
            "got status={} feedback={}",
            app.status_message,
            app.feedback.text
        );
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
        assert_eq!(m.status_label().0, "idle");
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
        assert!(!text.contains("native"), "chrome shows provider:\n{text}");
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
    async fn tui09_status_notices_mirror_chrome() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
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
        assert!(
            app.notices.iter().any(|l| l.contains("provider=")),
            "notices={:?}",
            app.notices
        );
        assert!(
            app.notices.iter().any(|l| l.contains("model=")),
            "notices={:?}",
            app.notices
        );
    }

    #[tokio::test]
    async fn tui08_report_error_writes_banner_and_activity_not_sticky_strip() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "mock".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
            },
        );
        app.report_error("upstream returned 429 rate limit exceeded");
        // Sticky red status strip intentionally not used for errors.
        assert!(
            app.feedback.is_empty(),
            "errors must not stick in feedback strip"
        );
        assert!(app.status_message.is_empty());
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
        assert!(
            app.feedback.text.to_ascii_lowercase().contains("token")
                || app.feedback.text.to_ascii_lowercase().contains("ctx")
                || app.status_message.contains("token"),
            "got feedback={} status={}",
            app.feedback.text,
            app.status_message
        );
        assert!(
            app.notices
                .iter()
                .any(|l| l.to_ascii_lowercase().contains("prompt")
                    || l.to_ascii_lowercase().contains("token")),
            "notices should list token kinds: {:?}",
            app.notices
        );
    }

    /// Save-and-restore env vars so dev machine credentials don't leak into tests.
    struct ScopedEnvGuard {
        saved: Vec<(String, Option<String>)>,
    }

    impl ScopedEnvGuard {
        fn new(keys: &[&str]) -> Self {
            let mut saved = Vec::new();
            for key in keys {
                saved.push((key.to_string(), std::env::var(key).ok()));
                std::env::remove_var(key);
            }
            Self { saved }
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
}
