//! TUI startup, event loop and terminal lifecycle for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `run_tui` owns terminal setup and teardown;
//! `run_loop` polls workspace/run state and drains pending work between frames.
//! Methods are moved verbatim.

use super::*;

impl TuiApp {
    pub(super) fn poll_interactive_terminal(&mut self) {
        if let Some(terminal) = self.interactive_terminal.as_mut() {
            terminal.poll();
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

/// Drain every pending terminal event (paste floods many keys; do not drop them).
pub(super) async fn drain_events(
    app: &mut TuiApp,
    mut terminal: Option<&mut Terminal<CrosstermBackend<io::Stdout>>>,
) -> Result<(), TuiError> {
    loop {
        if !event::poll(Duration::from_millis(0))? {
            break;
        }
        match event::read()? {
            Event::Key(key) => {
                app.handle_key(key).await?;
                app.invalidate_hit_regions();
            }
            Event::Mouse(mouse) => app.handle_mouse(mouse).await?,
            Event::Paste(data) => {
                app.handle_paste(&data);
                app.invalidate_hit_regions();
            }
            Event::Resize(_, _) => {
                if let Some(term) = terminal.as_deref_mut() {
                    term.autoresize()?;
                }
                app.invalidate_hit_regions();
            }
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
    run_tui_inner(session, runtime, None).await
}

/// Run the TUI with a startup session picker. The temporary session created
/// before entering the TUI is removed after the picker is cancelled or a
/// previous session is selected.
pub async fn run_tui_with_resume_picker(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
    items: Vec<ResumeSessionItem>,
) -> Result<ExitSummary, TuiError> {
    run_tui_inner(session, runtime, Some(items)).await
}

async fn run_tui_inner(
    session: AgentSession,
    runtime: TuiRuntimeConfig,
    startup_items: Option<Vec<ResumeSessionItem>>,
) -> Result<ExitSummary, TuiError> {
    enable_raw_mode()?;
    // Ensure the terminal is restored on panic, returned errors and normal exit.
    let _guard = TerminalGuard::install();
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    if runtime.mouse_capture {
        execute!(stdout, EnableMouseCapture)?;
    }
    execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        )
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = TuiApp::new_with_startup_resume_picker(session, runtime, startup_items);
    if app.overlay.is_none() && !app.is_provider_connected() {
        app.overlay = Some(Overlay::welcome());
        app.set_feedback(
            FeedbackSeverity::Info,
            "Welcome · connect a provider to start chatting",
        );
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
        app.poll_run();
        app.warm_catalog_once_connected();
        app.poll_catalog_refresh();
        app.poll_background_tasks().await?;
        app.tick_toast();
        app.tick_feedback();
        app.tick_notices();
        app.drain_auto_hitl().await?;
        app.maybe_open_hitl();
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
        if app.run_execution.execution.pending_validation {
            app.drain_pending_validation(Some(terminal)).await?;
            continue;
        }

        if event::poll(Duration::from_millis(200))? {
            // Read the ready event, then drain the rest of the queue so a paste
            // of a long API key is not truncated to a handful of characters.
            match event::read()? {
                Event::Key(key) => {
                    app.handle_key(key).await?;
                    app.invalidate_hit_regions();
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse).await?,
                Event::Paste(data) => {
                    app.handle_paste(&data);
                    app.invalidate_hit_regions();
                }
                Event::Resize(_, _) => {
                    app.invalidate_hit_regions();
                }
                _ => {}
            }
            drain_events(app, Some(terminal)).await?;
            app.poll_interactive_terminal();
            // Repaint immediately after input so theme and other state changes are visible
            // without waiting for the next idle frame.
            terminal.draw(|f| app.draw(f))?;
        }
    }
    Ok(())
}
