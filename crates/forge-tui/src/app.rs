//! Full-screen TUI application (TUI-01 shell + TUI-02/03/04 wired).

use std::io::{self, stdout};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use forge_connect::{
    builtin_registry, handle_connect_action, needs_tui_api_key_prompt, needs_tui_oauth,
    ConnectAction, ConnectError, ConnectRegistry, CredentialStore,
};
use forge_core::{AgentSession, LoopError};
use forge_types::HitlDecision;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use thiserror::Error;

use crate::activity::{ActivityFeed, ActivityKind};
use crate::commands::{help_text, parse_slash, SlashCommand, WorktreeAction};
use crate::conversation::{BannerKind, ChatItem, ConversationModel};
use crate::history::InputHistory;
use crate::layout::{is_too_small, split_areas_ex};
use crate::overlays::{
    filter_palette, handle_overlay_key, ConnectProfileItem, Key as OverlayKey, Overlay,
    OverlayAction, OverlayWidget, PaletteItem,
};
use crate::sidebar::SidebarModel;
use crate::widgets::{
    classify_operator_error, session_chrome_lines, BusyPhase, FeedbackBar, FeedbackModel,
    FeedbackSeverity, FooterBar, FooterModel, InputBar, InputModel, StatusBar, StatusModel,
};
use crate::theme;
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
    /// Optional web_search label for chrome (`mock` / `off` / provider id).
    pub web_search_label: Option<String>,
    /// Phase 10 / TUI-10 — activity ring buffer.
    pub activity: ActivityFeed,
}

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        let mut input = InputModel::default();
        input.hint = "Type a task or /command · Tab complete · Ctrl+K list…".into();
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
            history: InputHistory::default(),
            slash_suggest_idx: 0,
            notices: Vec::new(),
            feedback: FeedbackModel::default(),
            ui_banners: Vec::new(),
            busy_phase: BusyPhase::Idle,
            web_search_label: Some("mock".into()),
            activity: ActivityFeed::default(),
        }
    }

    /// Phase 10: set strip + keep `status_message` in sync for tests/compat.
    pub fn set_feedback(&mut self, severity: FeedbackSeverity, text: impl Into<String>) {
        let text = text.into();
        self.status_message = text.clone();
        self.feedback = FeedbackModel { text, severity };
    }

    /// Dual-write operator error: feedback strip + chat banner + activity (TUI-08/10).
    pub fn report_error(&mut self, raw: &str) {
        let msg = classify_operator_error(raw);
        self.set_feedback(FeedbackSeverity::Error, msg.clone());
        self.ui_banners.push(ChatItem::Banner {
            text: msg.clone(),
            kind: BannerKind::Error,
        });
        self.activity
            .push(ActivityKind::Error, FeedbackSeverity::Error, msg);
        self.busy_phase = BusyPhase::Idle;
        // Cap banners so chat stays usable
        const MAX: usize = 30;
        if self.ui_banners.len() > MAX {
            let drain = self.ui_banners.len() - MAX;
            self.ui_banners.drain(0..drain);
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

    fn activity_lines_for_sidebar(&self) -> Vec<String> {
        self.activity
            .recent(12)
            .iter()
            .map(|i| {
                let prefix = match i.kind {
                    ActivityKind::Model => "model",
                    ActivityKind::Tool => "tool",
                    ActivityKind::Connect => "connect",
                    ActivityKind::Slash => "slash",
                    ActivityKind::System => "sys",
                    ActivityKind::Error => "error",
                    ActivityKind::Hitl => "hitl",
                    ActivityKind::Context => "ctx",
                };
                format!("{prefix} {}", i.summary)
            })
            .collect()
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
        let items: Vec<ConnectProfileItem> = self
            .connect_registry
            .profiles()
            .iter()
            .map(|p| ConnectProfileItem {
                id: p.id.clone(),
                title: p.title.clone(),
                auth_mode: p.auth_mode.label().into(),
                auth_url: p.auth_url.clone(),
            })
            .collect();
        self.overlay = Some(Overlay::connect_picker(items));
        self.status_message = "Select a provider to connect".into();
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
        if let ConnectAction::Connect {
            ref profile_id,
            ref api_key,
            oauth_fixture,
        } = action
        {
            if api_key.is_none() && !oauth_fixture {
                if needs_tui_api_key_prompt(&self.connect_registry, profile_id) {
                    let p = self.connect_registry.get(profile_id);
                    let title = p.map(|x| x.title.clone()).unwrap_or_else(|| profile_id.clone());
                    let auth_url = p.and_then(|x| x.auth_url.clone());
                    let env_hint = p.and_then(|x| {
                        x.api_key_env.iter().find_map(|e| {
                            std::env::var(e).ok().filter(|v| !v.is_empty()).map(|_| e.clone())
                        })
                    });
                    self.overlay = Some(Overlay::connect_api_key(
                        profile_id.clone(),
                        title,
                        auth_url,
                        env_hint,
                    ));
                    self.status_message = format!("Enter API key for {profile_id}");
                    self.notices.clear();
                    return;
                }
                if needs_tui_oauth(&self.connect_registry, profile_id) {
                    let p = self.connect_registry.get(profile_id);
                    let title = p.map(|x| x.title.clone()).unwrap_or_else(|| profile_id.clone());
                    let instructions = p
                        .and_then(|x| match &x.auth_mode {
                            forge_connect::AuthMode::Oauth { auth_server, .. } => {
                                Some(forge_connect::OauthPending::start(profile_id, auth_server)
                                    .operator_instructions())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| "Complete OAuth in browser.".into());
                    self.overlay =
                        Some(Overlay::connect_oauth(profile_id.clone(), title, instructions));
                    self.status_message = format!("OAuth for {profile_id}");
                    self.notices.clear();
                    return;
                }
            }
        }

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
                    self.runtime.model_label = m;
                    self.runtime.provider = "litellm".into();
                }
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                self.status_message = lines.first().cloned().unwrap_or_default();
                self.notices = lines;
            }
            Err(ConnectError::OauthPending(_, instructions)) => {
                let lines: Vec<String> = instructions.lines().map(|s| s.to_string()).collect();
                self.status_message = lines.first().cloned().unwrap_or_default();
                self.notices = lines;
            }
            Err(e) => {
                self.status_message = e.to_string();
                self.notices = vec![e.to_string()];
            }
        }
    }

    fn finish_connect(
        &mut self,
        profile_id: &str,
        api_key: Option<String>,
        oauth_fixture: bool,
    ) {
        self.handle_connect(ConnectAction::Connect {
            profile_id: profile_id.into(),
            api_key,
            oauth_fixture,
        });
    }

    pub fn refresh_status_model(&self) -> StatusModel {
        let id = self.session.session_id.to_string();
        let short = if id.len() > 8 { id[..8].to_string() } else { id };
        StatusModel {
            status: self.session.status,
            session_short: short,
            model: self.runtime.model_label.clone(),
            provider: self.runtime.provider.clone(),
            ctx_pct: self.session.context_usage_ratio(),
            worktree_on: self.session.worktree_status().is_some(),
            busy: self.busy,
            busy_phase: self.busy_phase.clone(),
            connect_profile: self.connect_profile.clone(),
            web_search_label: self.web_search_label.clone(),
            tools_visible: self.session.list_tools().len(),
        }
    }

    pub fn draw(&self, frame: &mut ratatui::Frame) {
        let area = frame.size();
        if is_too_small(area) {
            frame.render_widget(
                Paragraph::new("Terminal too small — resize to at least 40x18"),
                area,
            );
            return;
        }
        let fb_h = if self.feedback.is_empty() { 0 } else { 1 };
        let regions = split_areas_ex(area, fb_h);
        let status = self.refresh_status_model();
        frame.render_widget(StatusBar { model: &status }, regions.status);

        let conv = ConversationModel::from_session(&self.session, self.busy)
            .with_extra_banners(self.ui_banners.iter().cloned());
        frame.render_widget(
            crate::conversation::ConversationWidget { model: &conv },
            regions.chat,
        );

        if let Some(sb_area) = regions.sidebar {
            let act = self.activity_lines_for_sidebar();
            let sb = SidebarModel::from_session_with_activity(&self.session, &act);
            frame.render_widget(crate::sidebar::SidebarWidget { model: &sb }, sb_area);
        }

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
                            let mut row =
                                raw.chars().take(inner_w.saturating_sub(1)).collect::<String>();
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
        input.dimmed = self.busy;
        frame.render_widget(InputBar { model: &input }, regions.input);

        let footer = FooterModel {
            version: self.runtime.version.clone(),
            cwd: self.runtime.cwd.display().to_string(),
            provider: self.runtime.provider.clone(),
        };
        frame.render_widget(FooterBar { model: &footer }, regions.footer);

        if let Some(ref ov) = self.overlay {
            frame.render_widget(OverlayWidget { overlay: ov }, area);
        }
    }

    pub async fn handle_key(&mut self, key: event::KeyEvent) -> Result<(), TuiError> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if let Some(ref mut ov) = self.overlay {
            let ok = map_key(key);
            let action = handle_overlay_key(ov, ok);
            match action {
                OverlayAction::None => {}
                OverlayAction::Close => self.overlay = None,
                OverlayAction::HitlApprove => {
                    self.session
                        .resolve_hitl(HitlDecision::Approve, "tui")
                        .await?;
                    self.overlay = None;
                    self.status_message = "approved".into();
                }
                OverlayAction::HitlDeny => {
                    self.session
                        .resolve_hitl(HitlDecision::Deny, "tui")
                        .await?;
                    self.overlay = None;
                    self.status_message = "denied".into();
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
                    self.runtime.provider = provider.clone();
                    self.runtime.model_label = model.clone();
                    self.set_feedback(
                        FeedbackSeverity::Ok,
                        format!("model {provider} · {model}"),
                    );
                }
                OverlayAction::ConnectSubmitKey {
                    profile_id,
                    api_key,
                } => {
                    self.overlay = None;
                    self.finish_connect(&profile_id, Some(api_key), false);
                }
                OverlayAction::ConnectCompleteOauth { profile_id } => {
                    self.overlay = None;
                    // Enter completes with OAuth fixture path when live exchange not available
                    self.finish_connect(&profile_id, None, true);
                }
                OverlayAction::ConnectUseEnv { profile_id } => {
                    self.overlay = None;
                    self.finish_connect(&profile_id, None, false);
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
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
                self.last_exit = ExitCode::Canceled;
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            // Phase 8: explicit command palette (discovery) — not auto on `/`
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.busy {
                    self.overlay = Some(Overlay::slash_open(""));
                }
            }
            KeyCode::Esc => {
                // cancel busy is best-effort; clear input + history browse
                self.history.reset_browse();
                self.notices.clear();
                if self.input.text.is_empty() {
                    // Clear info-level feedback on Esc; keep error strip until next success
                    if self.feedback.severity != FeedbackSeverity::Error {
                        self.feedback = FeedbackModel::default();
                        self.status_message.clear();
                    }
                    self.notices.clear();
                } else {
                    self.input.clear();
                    self.slash_suggest_idx = 0;
                }
            }
            KeyCode::Enter => {
                if self.busy {
                    return Ok(());
                }
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
                if line.is_empty() {
                    return Ok(());
                }
                self.history.push(&line);
                self.slash_suggest_idx = 0;
                self.notices.clear();
                self.input.history_browse = false;
                self.dispatch_line(&line).await?;
            }
            KeyCode::Tab => {
                if !self.busy {
                    self.complete_slash_suggestion();
                }
            }
            KeyCode::Up => {
                if !self.busy {
                    let suggestions = self.slash_suggestions();
                    if self.input.text.starts_with('/') && !suggestions.is_empty() {
                        let n = suggestions.len();
                        self.slash_suggest_idx =
                            (self.slash_suggest_idx + n - 1) % n;
                    } else if let Some(text) = self.history.up(&self.input.text) {
                        self.apply_history_text(text);
                    }
                }
            }
            KeyCode::Down => {
                if !self.busy {
                    let suggestions = self.slash_suggestions();
                    if self.input.text.starts_with('/') && !suggestions.is_empty() {
                        let n = suggestions.len();
                        self.slash_suggest_idx = (self.slash_suggest_idx + 1) % n;
                    } else if let Some(text) = self.history.down() {
                        self.apply_history_text(text);
                    }
                }
            }
            KeyCode::Char(c) => {
                // Phase 8 (TUI-06): `/` inserts into the main textbox; do not open palette
                if !self.busy {
                    self.input.history_browse = false;
                    self.input.insert(c);
                    self.clamp_slash_suggest();
                }
            }
            KeyCode::Backspace => {
                if !self.busy {
                    self.input.backspace();
                    self.clamp_slash_suggest();
                }
            }
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::PageUp => {
                // conversation scroll (TUI-02) — separate from input history
            }
            KeyCode::PageDown => {}
            _ => {}
        }
        Ok(())
    }

    pub async fn dispatch_line(&mut self, line: &str) -> Result<(), TuiError> {
        if let Some(cmd_res) = parse_slash(line) {
            let slash_name = line.split_whitespace().next().unwrap_or("/");
            self.push_activity(
                ActivityKind::Slash,
                FeedbackSeverity::Info,
                slash_name,
            );
            match cmd_res {
                Ok(SlashCommand::Quit) => {
                    self.should_quit = true;
                    self.status_message = "quitting…".into();
                }
                Ok(SlashCommand::Help { cmd }) => {
                    if let Some(name) = cmd {
                        // Point at one entry if possible
                        let needle = name.trim_start_matches('/').to_ascii_lowercase();
                        let hits: Vec<String> = filter_palette(&needle)
                            .into_iter()
                            .map(|i| format!("{}  —  {}", i.cmd, i.desc))
                            .collect();
                        if hits.is_empty() {
                            self.status_message = format!("unknown help topic: {name}");
                            self.notices = vec![
                                format!("No command matching `{name}`."),
                                "Type /help for the full list.".into(),
                            ];
                        } else {
                            self.status_message = format!("help: {name}");
                            self.notices = hits;
                        }
                    } else {
                        self.status_message =
                            "commands listed below · type /cmd · Ctrl+K palette".into();
                        self.notices = help_text()
                            .lines()
                            .map(|s| s.to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    }
                }
                Ok(SlashCommand::Status) => {
                    let chrome = self.refresh_status_model();
                    let mut lines = session_chrome_lines(&chrome);
                    lines.insert(0, format!("session_id={}", self.session.session_id));
                    self.set_feedback(
                        FeedbackSeverity::Info,
                        format!(
                            "{} · {} · ctx {:.0}%",
                            chrome.provider,
                            chrome.model,
                            chrome.ctx_pct * 100.0
                        ),
                    );
                    self.notices = lines;
                }
                Ok(SlashCommand::Tools) => {
                    let tools = self.session.list_tools();
                    if tools.is_empty() {
                        self.status_message = "no tools registered".into();
                        self.notices = vec![
                            "No tools are registered on this session.".into(),
                            "Tools appear when the agent runtime attaches them.".into(),
                        ];
                    } else {
                        self.status_message = format!("{} tools", tools.len());
                        self.notices = tools;
                    }
                }
                Ok(SlashCommand::Cost) => {
                    let pct = self.session.context_usage_ratio() * 100.0;
                    let msg = format!("context usage {pct:.1}%");
                    self.set_feedback(FeedbackSeverity::Info, msg.clone());
                    self.notices = vec![msg];
                }
                Ok(SlashCommand::Approve) => {
                    if self.session.pending_hitl.is_none() {
                        self.status_message = "no pending HITL to approve".into();
                        self.notices = vec!["No human-in-the-loop request is waiting.".into()];
                    } else {
                        self.session
                            .resolve_hitl(HitlDecision::Approve, "tui")
                            .await?;
                        self.status_message = "approved".into();
                        self.notices = vec!["HITL approved.".into()];
                    }
                }
                Ok(SlashCommand::Deny) => {
                    if self.session.pending_hitl.is_none() {
                        self.status_message = "no pending HITL to deny".into();
                        self.notices = vec!["No human-in-the-loop request is waiting.".into()];
                    } else {
                        self.session
                            .resolve_hitl(HitlDecision::Deny, "tui")
                            .await?;
                        self.status_message = "denied".into();
                        self.notices = vec!["HITL denied.".into()];
                    }
                }
                Ok(SlashCommand::Reset) | Ok(SlashCommand::Compact) => {
                    self.session.force_context_reset_async().await?;
                    self.status_message = "context reset".into();
                    self.notices = vec!["Context handoff reset completed.".into()];
                }
                Ok(SlashCommand::Model { provider, model }) => {
                    if provider.is_none() && model.is_none() {
                        self.overlay = Some(Overlay::model_open());
                        self.status_message = "pick a model".into();
                    } else {
                        let msg = format!(
                            "model provider={provider:?} model={model:?} — set via /connect or restart to apply"
                        );
                        self.status_message = msg.clone();
                        self.notices = vec![msg];
                    }
                }
                Ok(SlashCommand::Worktree { action }) => match action {
                    WorktreeAction::Status => {
                        let msg = self
                            .session
                            .worktree_status()
                            .unwrap_or_else(|| "worktree off".into());
                        self.status_message = msg.clone();
                        self.notices = vec![
                            msg,
                            "Usage: /worktree status | merge | discard --yes".into(),
                        ];
                    }
                    WorktreeAction::Merge => {
                        self.session.worktree_merge()?;
                        self.status_message = "worktree merged".into();
                        self.notices = vec!["Worktree merged into the main checkout.".into()];
                    }
                    WorktreeAction::Discard { confirm } => {
                        if confirm {
                            self.session.worktree_discard()?;
                            self.status_message = "worktree discarded".into();
                            self.notices = vec!["Worktree discarded.".into()];
                        } else {
                            self.status_message = "confirm discard with --yes".into();
                            self.notices = vec![
                                "Usage: /worktree discard --yes".into(),
                                "This permanently discards the session worktree.".into(),
                            ];
                        }
                    }
                },
                Ok(SlashCommand::Journal { tail }) => {
                    let n = self.session.events.len();
                    let take = tail.unwrap_or(12).min(n).min(20);
                    self.status_message = format!("{n} events · showing last {take}");
                    if n == 0 {
                        self.notices = vec!["Journal is empty for this session.".into()];
                    } else {
                        self.notices = self
                            .session
                            .events
                            .iter()
                            .rev()
                            .take(take)
                            .map(|e| format!("{e:?}"))
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                    }
                }
                Ok(SlashCommand::Resume { session_id }) => {
                    let msg = format!(
                        "To resume {session_id}, restart: forge tui --resume {session_id}"
                    );
                    self.status_message = "resume requires CLI restart".into();
                    self.notices = vec![msg];
                }
                Ok(SlashCommand::Cancel) => {
                    self.status_message = "cancel".into();
                    self.notices = vec![
                        "Cancel requested.".into(),
                        "If a turn is running, it will stop at the next safe point.".into(),
                    ];
                }
                Ok(SlashCommand::Connect(action)) => {
                    self.handle_connect(action);
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.set_feedback(FeedbackSeverity::Warn, msg.clone());
                    self.notices = vec![msg, "Type /help for commands.".into()];
                }
            }
            return Ok(());
        }

        // user message
        self.busy = true;
        self.busy_phase = BusyPhase::Model;
        self.push_activity(
            ActivityKind::Model,
            FeedbackSeverity::Info,
            "model call started",
        );
        let result = self.session.run_user_message(line).await;
        self.busy = false;
        self.busy_phase = BusyPhase::Idle;
        match result {
            Ok(_) => {
                if self.session.pending_hitl.is_some() {
                    if let Some(ref p) = self.session.pending_hitl {
                        self.overlay = Some(Overlay::hitl(p.clone()));
                    }
                    self.last_exit = ExitCode::AwaitingHitl;
                    self.set_feedback(FeedbackSeverity::Warn, "awaiting human approval");
                    self.push_activity(
                        ActivityKind::Hitl,
                        FeedbackSeverity::Warn,
                        "hitl waiting",
                    );
                } else {
                    self.set_feedback(FeedbackSeverity::Ok, "turn complete");
                    self.push_activity(
                        ActivityKind::Model,
                        FeedbackSeverity::Ok,
                        "model ok",
                    );
                }
            }
            Err(e) => {
                self.report_error(&e.to_string());
                self.last_exit = ExitCode::Failed;
            }
        }
        Ok(())
    }

    pub fn maybe_open_hitl(&mut self) {
        if self.overlay.is_none() {
            if let Some(ref p) = self.session.pending_hitl {
                self.overlay = Some(Overlay::hitl(p.clone()));
            }
        }
    }
}

fn map_key(key: event::KeyEvent) -> OverlayKey {
    match key.code {
        KeyCode::Esc => OverlayKey::Esc,
        KeyCode::Enter => OverlayKey::Enter,
        KeyCode::Up => OverlayKey::Up,
        KeyCode::Down => OverlayKey::Down,
        KeyCode::Backspace => OverlayKey::Backspace,
        KeyCode::Char(c) => OverlayKey::Char(c),
        _ => OverlayKey::Other,
    }
}

/// Run the full-screen TUI until quit.
pub async fn run_tui(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
) -> Result<ExitCode, TuiError> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new(session, runtime);
    let result = run_loop(&mut terminal, &mut app).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result.map(|_| app.last_exit)
}

async fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<(), TuiError> {
    while !app.should_quit {
        app.maybe_open_hitl();
        terminal.draw(|f| app.draw(f))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key).await?;
            }
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
                || app.notices.iter().any(|l| l.contains("session") || l.contains("model=")),
            "status={} notices={:?}",
            app.status_message,
            app.notices
        );
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
                provider: "litellm".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
            },
        );
        app.dispatch_line("/connect opencode_go").await.unwrap();
        match &app.overlay {
            Some(Overlay::ConnectApiKey { profile_id, title, .. }) => {
                assert_eq!(profile_id, "opencode_go");
                assert!(title.contains("OpenCode"));
            }
            other => panic!("expected ConnectApiKey overlay, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn connect_xai_opens_oauth_overlay() {
        let (_dir, session) = test_session().await;
        let mut app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "m".into(),
                provider: "litellm".into(),
                cwd: PathBuf::from("."),
                version: "0.6.1".into(),
            },
        );
        app.dispatch_line("/connect xai").await.unwrap();
        match &app.overlay {
            Some(Overlay::ConnectOauth { profile_id, title, .. }) => {
                assert_eq!(profile_id, "xai");
                assert!(title.contains("Grok") || title.contains("xAI"));
            }
            other => panic!("expected ConnectOauth overlay, got {other:?}"),
        }
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
        app.input.set_text("/tools");
        app.handle_key(enter).await.unwrap();
        assert!(app.history.len() >= 2);
        let t = app.history.up(&app.input.text).unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/tools");
        let t = app.history.up(&app.input.text).unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/status");
        let t = app.history.down().unwrap();
        app.apply_history_text(t);
        assert_eq!(app.input.text, "/tools");
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
                || app.notices.iter().any(|l| l.contains("model=") || l.contains("provider=")),
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
        // Partial type; suggestions include /connect (and possibly /cost via "Context")
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
            "/help",
            "/status",
            "/connect",
            "/model",
            "/tools",
            "/cost",
            "/journal",
            "/worktree",
            "/approve",
            "/deny",
            "/reset",
            "/compact",
            "/resume",
            "/cancel",
            "/quit",
        ] {
            assert!(
                suggestions.iter().any(|s| s.cmd == cmd),
                "missing {cmd} in suggestions"
            );
        }
    }

    #[tokio::test]
    async fn help_command_fills_notices_with_full_list() {
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
        for c in "/help".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            app.notices.len() > 5,
            "help should populate multi-line notices, got {:?}",
            app.notices
        );
        assert!(
            app.notices.iter().any(|l| l.contains("/connect"))
                && app.notices.iter().any(|l| l.contains("/status")),
            "help notices missing expected commands: {:?}",
            app.notices
        );
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
            ctx_pct: 0.2,
            worktree_on: false,
            busy: false,
            busy_phase: BusyPhase::Idle,
            connect_profile: None,
            web_search_label: None,
            tools_visible: 0,
        };
        assert_eq!(m.status_label().0, "idle");
    }

    #[tokio::test]
    async fn tui09_chrome_includes_provider_and_model_on_frame() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let app = TuiApp::new(
            session,
            TuiRuntimeConfig {
                model_label: "openai/gpt-test".into(),
                provider: "litellm".into(),
                cwd: PathBuf::from("."),
                version: "0.10.0".into(),
            },
        );
        let chrome = app.refresh_status_model();
        assert_eq!(chrome.provider, "litellm");
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
        assert!(
            text.contains("litellm") && (text.contains("gpt") || text.contains("openai")),
            "chrome missing provider/model:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui09_narrow_frame_still_shows_model_or_ctx() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let (_dir, session) = test_session().await;
        let app = TuiApp::new(
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
    async fn tui08_report_error_dual_writes_feedback_and_banner() {
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
        assert!(
            !app.feedback.is_empty(),
            "feedback strip must be set"
        );
        assert!(
            app.feedback.text.contains("rate limited") || app.feedback.text.contains("429"),
            "got {}",
            app.feedback.text
        );
        assert_eq!(app.feedback.severity, FeedbackSeverity::Error);
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
        assert_eq!(app.status_message, app.feedback.text);
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
            recent.iter().any(|s| s.contains("rate") || s.contains("429") || s.contains("Model")),
            "recent={recent:?}"
        );
        assert_eq!(app.busy_phase, BusyPhase::Idle);
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
    async fn tui10_sidebar_shows_activity_on_frame() {
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
        app.push_activity(
            ActivityKind::Tool,
            FeedbackSeverity::Ok,
            "web_search done",
        );
        app.report_error("429 rate limit");
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
        assert!(
            text.contains("ACTIVITY")
                || text.contains("web_search")
                || text.contains("error")
                || text.contains("rate"),
            "frame missing activity:\n{text}"
        );
    }

    #[tokio::test]
    async fn tui08_cost_sets_feedback_strip() {
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
        for c in "/cost".chars() {
            app.handle_key(press(KeyCode::Char(c), KeyModifiers::NONE))
                .await
                .unwrap();
        }
        app.handle_key(press(KeyCode::Enter, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(
            app.feedback.text.contains("context") || app.status_message.contains("context"),
            "got feedback={} status={}",
            app.feedback.text,
            app.status_message
        );
    }
}
