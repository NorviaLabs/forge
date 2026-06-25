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

use crate::commands::{parse_slash, SlashCommand, WorktreeAction};
use crate::conversation::ConversationModel;
use crate::layout::{is_too_small, split_areas};
use crate::overlays::{
    handle_overlay_key, Key as OverlayKey, Overlay, OverlayAction, OverlayWidget,
};
use crate::sidebar::SidebarModel;
use crate::widgets::{
    FooterBar, FooterModel, InputBar, InputModel, StatusBar, StatusModel,
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
}

impl TuiApp {
    pub fn new(session: AgentSession, runtime: TuiRuntimeConfig) -> Self {
        let mut input = InputModel::default();
        input.hint = "Describe a task or / for commands…".into();
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
        }
    }

    fn handle_connect(&mut self, action: ConnectAction) {
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
                self.status_message = if msg.lines().count() > 1 {
                    msg.replace('\n', " · ")
                } else {
                    msg
                };
            }
            Err(ConnectError::OauthPending(_, instructions)) => {
                self.status_message = instructions.replace('\n', " · ");
            }
            Err(e) => {
                self.status_message = e.to_string();
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
            KeyCode::Esc => {
                // cancel busy is best-effort; clear input
                if self.input.text.is_empty() {
                    self.status_message = "esc".into();
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Enter => {
                if self.busy {
                    return Ok(());
                }
                let line = self.input.take();
                if line.is_empty() {
                    return Ok(());
                }
                self.dispatch_line(&line).await?;
            }
            KeyCode::Char('/') if self.input.text.is_empty() => {
                self.overlay = Some(Overlay::slash_open(""));
            }
            KeyCode::Char(c) => {
                if !self.busy {
                    self.input.insert(c);
                    if self.input.text == "/" {
                        self.input.clear();
                        self.overlay = Some(Overlay::slash_open(""));
                    }
                }
            }
            KeyCode::Backspace => {
                if !self.busy {
                    self.input.backspace();
                }
            }
            KeyCode::Left => self.input.move_left(),
            KeyCode::Right => self.input.move_right(),
            KeyCode::PageUp => {
                // scroll handled via conversation model — store on app if needed
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
                }
                Ok(SlashCommand::Help { .. }) => {
                    self.status_message = "see / palette for commands".into();
                    self.overlay = Some(Overlay::slash_open(""));
                }
                Ok(SlashCommand::Status) => {
                    self.status_message = format!(
                        "session={} status={:?} tools={}",
                        self.session.session_id,
                        self.session.status,
                        self.session.list_tools().len()
                    );
                }
                Ok(SlashCommand::Tools) => {
                    self.status_message = self.session.list_tools().join(", ");
                }
                Ok(SlashCommand::Cost) => {
                    self.status_message = format!(
                        "ctx {:.1}%",
                        self.session.context_usage_ratio() * 100.0
                    );
                }
                Ok(SlashCommand::Approve) => {
                    self.session
                        .resolve_hitl(HitlDecision::Approve, "tui")
                        .await?;
                }
                Ok(SlashCommand::Deny) => {
                    self.session
                        .resolve_hitl(HitlDecision::Deny, "tui")
                        .await?;
                }
                Ok(SlashCommand::Reset) | Ok(SlashCommand::Compact) => {
                    self.session.force_context_reset_async().await?;
                    self.status_message = "context reset".into();
                }
                Ok(SlashCommand::Model { provider, model }) => {
                    if provider.is_none() && model.is_none() {
                        self.overlay = Some(Overlay::model_open());
                    } else {
                        self.status_message = format!(
                            "model {:?} {:?} — restart to apply",
                            provider, model
                        );
                    }
                }
                Ok(SlashCommand::Worktree { action }) => match action {
                    WorktreeAction::Status => {
                        self.status_message = self
                            .session
                            .worktree_status()
                            .unwrap_or_else(|| "worktree off".into());
                    }
                    WorktreeAction::Merge => {
                        self.session.worktree_merge()?;
                        self.status_message = "worktree merged".into();
                    }
                    WorktreeAction::Discard { confirm } => {
                        if confirm {
                            self.session.worktree_discard()?;
                            self.status_message = "worktree discarded".into();
                        } else {
                            self.status_message = "use /worktree discard --yes".into();
                        }
                    }
                },
                Ok(SlashCommand::Journal { .. }) => {
                    let n = self.session.events.len();
                    self.status_message = format!("{n} recent events in sidebar");
                }
                Ok(SlashCommand::Resume { session_id }) => {
                    self.status_message =
                        format!("resume {session_id} — restart forge tui --resume");
                }
                Ok(SlashCommand::Cancel) => {
                    self.status_message = "cancel".into();
                }
                Ok(SlashCommand::Connect(action)) => {
                    self.handle_connect(action);
                }
                Err(e) => {
                    self.status_message = e.to_string();
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
