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

use crate::commands::{help_text, parse_slash, SlashCommand, WorktreeAction};
use crate::conversation::ConversationModel;
use crate::history::InputHistory;
use crate::layout::{is_too_small, split_areas};
use crate::overlays::{
    filter_palette, handle_overlay_key, ConnectProfileItem, Key as OverlayKey, Overlay,
    OverlayAction, OverlayWidget, PaletteItem,
};
use crate::sidebar::SidebarModel;
use crate::widgets::{
    FooterBar, FooterModel, InputBar, InputModel, StatusBar, StatusModel,
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
        }
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
            ctx_pct: self.session.context_usage_ratio(),
            worktree_on: self.session.worktree_status().is_some(),
            busy: self.busy,
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
        let regions = split_areas(area);
        let status = self.refresh_status_model();
        frame.render_widget(StatusBar { model: &status }, regions.status);

        let conv = ConversationModel::from_session(&self.session, self.busy);
        frame.render_widget(
            crate::conversation::ConversationWidget { model: &conv },
            regions.chat,
        );

        if let Some(sb_area) = regions.sidebar {
            let sb = SidebarModel::from_session(&self.session);
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
                    self.status_message =
                        format!("model {provider}/{model} — restart session to apply");
                    self.runtime.provider = provider;
                    self.runtime.model_label = model;
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
                    // Continue into oauth / api-key flow for the chosen profile
                    self.finish_connect(&profile_id, None, false);
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
                    self.status_message = "esc".into();
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
                    let msg = format!(
                        "session={} status={:?} tools={} model={} provider={}",
                        self.session.session_id,
                        self.session.status,
                        self.session.list_tools().len(),
                        self.runtime.model_label,
                        self.runtime.provider,
                    );
                    self.status_message = msg.clone();
                    self.notices = vec![msg];
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
                    self.status_message = msg.clone();
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
                    self.status_message = msg.clone();
                    self.notices = vec![msg, "Type /help for commands.".into()];
                }
            }
            return Ok(());
        }

        // user message
        self.busy = true;
        let result = self.session.run_user_message(line).await;
        self.busy = false;
        match result {
            Ok(_) => {
                if self.session.pending_hitl.is_some() {
                    if let Some(ref p) = self.session.pending_hitl {
                        self.overlay = Some(Overlay::hitl(p.clone()));
                    }
                    self.last_exit = ExitCode::AwaitingHitl;
                }
            }
            Err(e) => {
                self.status_message = e.to_string();
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
        assert!(app.status_message.contains("session="));
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
            app.status_message.contains("session="),
            "status={}",
            app.status_message
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
            app.status_message.contains("session="),
            "got {}",
            app.status_message
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
            ctx_pct: 0.2,
            worktree_on: false,
            busy: false,
        };
        assert_eq!(m.status_label().0, "idle");
    }
}
