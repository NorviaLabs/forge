//! TUI startup, event loop and terminal lifecycle for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `run_tui` owns terminal setup and teardown;
//! `run_loop` polls workspace/run state and drains pending work between frames.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn poll_interactive_terminal(&mut self) -> bool {
        if let Some(terminal) = self.interactive_terminal.as_mut() {
            terminal.poll()
        } else {
            false
        }
    }

    pub(super) fn resize_interactive_terminal(&mut self, width: u16, height: u16) {
        if let Some(terminal) = self.interactive_terminal.as_mut() {
            if let Err(error) = terminal.resize(width, height) {
                self.set_feedback(
                    FeedbackSeverity::Error,
                    format!("terminal resize failed: {error}"),
                );
            }
        }
    }
}

/// Keep a redraw cadence while draining a burst of terminal events.
///
/// An unbounded drain lets a continuous stream of ordinary key presses starve
/// the draw that follows it. That becomes especially visible when the composer
/// first wraps: the input has changed, but the screen does not update until the
/// user pauses. Bracketed paste is still delivered as one `Event::Paste`; older
/// terminals that emit a paste as key events are processed over successive
/// frames without dropping any input.
const MAX_EVENTS_PER_FRAME: usize = 8;

pub(super) async fn drain_events(
    app: &mut TuiApp,
    mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
) -> Result<(), TuiError> {
    for _ in 0..MAX_EVENTS_PER_FRAME {
        if !event::poll(Duration::from_millis(0))? {
            break;
        }
        match event::read()? {
            Event::Key(key) => {
                app.handle_key(key).await?;
            }
            Event::Mouse(event) => {
                app.handle_mouse(event).await?;
            }
            Event::Paste(data) => {
                app.handle_paste(&data);
            }
            Event::Resize(_, _) => {
                if let Some(term) = terminal.as_deref_mut() {
                    term.autoresize()?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Wait for user input without making PTY output wait for the full idle tick.
/// Crossterm only wakes for terminal input, while the interactive shell is
/// read by a separate thread. Check that channel in short slices and repaint
/// as soon as the reader has delivered a changed state.
fn wait_for_input_or_terminal_output(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut TuiApp,
) -> Result<bool, TuiError> {
    let deadline = Instant::now() + Duration::from_millis(200);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        if event::poll(remaining.min(Duration::from_millis(20)))? {
            return Ok(true);
        }
        if app.poll_interactive_terminal() {
            terminal.draw(|f| app.draw(f))?;
        }
    }
}

/// Extra launch flags for first-install / new-project / returning.
#[derive(Debug, Clone, Default)]
pub struct TuiLaunch {
    pub startup_items: Option<Vec<ResumeSessionItem>>,
    pub onboarding_connect: bool,
    pub ready_placeholder: bool,
}

/// Run the full-screen TUI until quit.
pub async fn run_tui(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
) -> Result<ExitSummary, TuiError> {
    run_tui_inner(session, runtime, TuiLaunch::default()).await
}

/// Run the TUI with a startup session picker. The temporary session created
/// before entering the TUI is removed after the picker is cancelled or a
/// previous session is selected.
pub async fn run_tui_with_resume_picker(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
    items: Vec<ResumeSessionItem>,
) -> Result<ExitSummary, TuiError> {
    run_tui_inner(
        session,
        runtime,
        TuiLaunch {
            startup_items: Some(items),
            ..TuiLaunch::default()
        },
    )
    .await
}

pub async fn run_tui_with_launch(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
    launch: TuiLaunch,
) -> Result<ExitSummary, TuiError> {
    run_tui_inner(session, runtime, launch).await
}

async fn run_tui_inner(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
    launch: TuiLaunch,
) -> Result<ExitSummary, TuiError> {
    enable_raw_mode()?;
    // Ensure the terminal is restored on panic, returned errors and normal exit.
    let _guard = TerminalGuard::install();
    let mut stdout = stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        SetCursorStyle::SteadyBlock,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new_with_startup_resume_picker(session, runtime, launch.startup_items);
    app.onboarding_connect = launch.onboarding_connect;
    if launch.ready_placeholder {
        app.input.hint = "What does this project do?".into();
    }
    if app.overlay.is_none() && launch.onboarding_connect && !app.is_provider_connected() {
        app.open_connect_picker();
        app.set_feedback(FeedbackSeverity::Info, "Connect a provider · Esc quits");
    }
    let result = run_loop(&mut terminal, &mut app).await;

    app.persist_selection();

    if let Some(session_id) = app.startup_resume.session_id {
        let path = app.session.journal_dir().join(format!("{session_id}.db"));
        let _ = std::fs::remove_file(path);
    }

    result.map(|_| {
        let report = app.session.token_usage_report();
        ExitSummary {
            exit_code: app.exit.code,
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
    while !app.exit.requested {
        app.poll_file_changes();
        app.poll_interactive_terminal();
        app.warm_catalog_once_connected();
        app.poll_catalog_refresh();
        app.poll_background_tasks().await?;
        app.tick_toast();
        app.tick_feedback();
        app.tick_notices();
        app.drain_auto_hitl().await?;
        // Newly arrived approvals claim focus + scroll-into-view once.
        app.sync_approval_focus();
        // Grok-style device-code: poll token endpoint while overlay is open
        app.poll_oauth_tick();
        terminal.draw(|f| app.draw(f))?;

        // Drain queued user prompt with streaming redraws (YOU paints before first token)
        if app.pending_turn.prompt.is_some() {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        if app.pending_turn.continue_turn {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        if app.pending_interaction.hitl_decision.is_some() {
            app.drain_pending_hitl(Some(terminal)).await?;
            continue;
        }
        if app.pending_interaction.context_reset {
            app.drain_pending_context_reset(Some(terminal)).await?;
            continue;
        }
        if app.external_editor.requested {
            app.drain_pending_external_editor(Some(terminal)).await?;
            continue;
        }

        let input_ready = if app.interactive_terminal.is_some() {
            wait_for_input_or_terminal_output(terminal, app)?
        } else {
            event::poll(Duration::from_millis(200))?
        };
        if input_ready {
            // Read the ready event, then drain the rest of the queue so a paste
            // of a long API key is not truncated to a handful of characters.
            match event::read()? {
                Event::Key(key) => {
                    app.handle_key(key).await?;
                }
                Event::Mouse(event) => {
                    app.handle_mouse(event).await?;
                }
                Event::Paste(data) => {
                    app.handle_paste(&data);
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
            drain_events(app, Some(terminal)).await?;
            app.poll_interactive_terminal();
            // Next loop iteration draws once after all input and background polls.
        }
    }
    Ok(())
}
