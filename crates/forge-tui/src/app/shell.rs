//! TUI startup, event loop and terminal lifecycle for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `run_tui` owns terminal setup and teardown;
//! `run_loop` polls workspace/run state and drains pending work between frames.
//! Methods are moved verbatim.

use super::*;

/// Drain every pending terminal event (paste floods many keys; do not drop them).
pub(super) async fn drain_events(app: &mut TuiApp) -> Result<(), TuiError> {
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
            Event::Resize(_, _) => app.invalidate_hit_regions(),
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
        app.poll_file_changes();
        app.poll_run();
        app.poll_background_tasks().await?;
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
        if app.run_exec.pending_validation {
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
                Event::Resize(_, _) => app.invalidate_hit_regions(),
                _ => {}
            }
            drain_events(app).await?;
            // Repaint immediately after input so theme and other state changes are visible
            // without waiting for the next idle frame.
            terminal.draw(|f| app.draw(f))?;
        }
    }
    Ok(())
}
