//! TUI startup, event loop and terminal lifecycle for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `run_tui` owns terminal setup and teardown;
//! `run_loop` polls workspace/run state and drains pending work between frames.
//! Methods are moved verbatim.

use super::*;
pub(crate) struct TerminalEventSource {
    rx: tokio::sync::mpsc::UnboundedReceiver<io::Result<Event>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TerminalEventSource {
    pub(super) fn spawn() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let task = tokio::task::spawn_blocking(move || {
            while !reader_stop.load(std::sync::atomic::Ordering::Acquire) {
                match event::poll(Duration::from_millis(20)) {
                    Ok(true) => match event::read() {
                        Ok(event) => {
                            if tx.send(Ok(event)).is_err() {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Err(error));
                            break;
                        }
                    },
                    Ok(false) => {}
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self {
            rx,
            stop,
            task: Some(task),
        }
    }

    async fn recv(&mut self) -> Option<io::Result<Event>> {
        self.rx.recv().await
    }

    #[cfg(not(test))]
    fn try_recv(&mut self) -> Result<io::Result<Event>, tokio::sync::mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }

    pub(super) async fn shutdown(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for TerminalEventSource {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(task) = self.task.as_ref() {
            task.abort();
        }
    }
}

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
const MAX_EVENTS_PER_FRAME: usize = 32;

pub(super) enum ForegroundWake {
    Input(Event),
    Tick,
}

async fn dispatch_terminal_event<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    event: Event,
    terminal: Option<&mut Terminal<B>>,
) -> Result<(), TuiError> {
    match event {
        Event::Key(key) => app.handle_key(key).await?,
        Event::Mouse(event) => app.handle_mouse(event).await?,
        Event::Paste(data) => app.handle_paste(&data),
        Event::Resize(_, _) => {
            if let Some(term) = terminal {
                term.autoresize()
                    .map_err(|error| TuiError::Other(error.to_string()))?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) async fn drain_events<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    mut terminal: Option<&mut Terminal<B>>,
) -> Result<(), TuiError> {
    for _ in 0..MAX_EVENTS_PER_FRAME {
        #[cfg(test)]
        let next = match app.test_events.pop_front() {
            Some(event) => event,
            None => break,
        };
        #[cfg(not(test))]
        let next = {
            let Some(events) = app.terminal_events.as_mut() else {
                break;
            };
            match events.try_recv() {
                Ok(event) => event?,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    return Err(TuiError::Other("terminal event stream closed".into()));
                }
            }
        };
        dispatch_terminal_event(app, next, terminal.as_deref_mut()).await?;
    }
    Ok(())
}

/// Wait until terminal input actually arrives or a time-based UI service is due.
/// Stream/model events use their own wake source and never wait for this ticker.
pub(super) async fn next_foreground_wake(
    app: &mut TuiApp,
    ticker: &mut tokio::time::Interval,
) -> Result<ForegroundWake, TuiError> {
    #[cfg(test)]
    {
        if let Some(event) = app.test_events.pop_front() {
            return Ok(ForegroundWake::Input(event));
        }
        ticker.tick().await;
        Ok(ForegroundWake::Tick)
    }

    #[cfg(not(test))]
    {
        let Some(events) = app.terminal_events.as_mut() else {
            ticker.tick().await;
            return Ok(ForegroundWake::Tick);
        };
        tokio::select! {
            event = events.recv() => {
                let event = event
                    .ok_or_else(|| TuiError::Other("terminal event stream closed".into()))??;
                Ok(ForegroundWake::Input(event))
            }
            _ = ticker.tick() => Ok(ForegroundWake::Tick),
        }
    }
}

/// Advance every non-blocking service owned by the TUI application.
///
/// A foreground model turn temporarily drives its own model/tool state machine,
/// but it must not replace the application loop while it waits. Keeping this
/// tick in one place makes foreground waits and the idle loop service the same
/// file watcher, background tasks, approvals, connection state and transient
/// chrome. Rendering and terminal input stay with the single Ratatui owner.
pub(super) async fn tick_application(app: &mut TuiApp) -> Result<(), TuiError> {
    app.poll_file_changes();
    app.poll_interactive_terminal();
    app.tick_render_state();
    app.warm_catalog_once_connected();
    app.poll_catalog_refresh();
    app.poll_background_tasks().await?;
    app.poll_approved_hitl().await?;
    app.tick_toast();
    app.tick_feedback();
    app.tick_notices();
    app.drain_auto_hitl().await?;
    // Newly arrived approvals claim focus + scroll-into-view once.
    app.sync_approval_focus();
    app.sync_question_focus();
    // Grok-style device-code: poll token endpoint while overlay is open.
    app.poll_oauth_tick();
    Ok(())
}

/// Run one complete TUI tick while foreground work is in flight, then consume
/// terminal input and paint the resulting state.
pub(super) async fn paint_foreground_frame<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    mut terminal: Option<&mut Terminal<B>>,
    service_application: bool,
) -> Result<(), TuiError> {
    if service_application {
        tick_application(app).await?;
    }
    if terminal.is_some() {
        drain_events(app, terminal.as_deref_mut()).await?;
        if let Some(term) = terminal {
            term.draw(|frame| app.draw(frame))
                .map_err(|error| TuiError::Other(error.to_string()))?;
        }
    }
    Ok(())
}

pub(super) async fn render_foreground_wake<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    mut terminal: Option<&mut Terminal<B>>,
    wake: ForegroundWake,
) -> Result<(), TuiError> {
    let service_application = matches!(wake, ForegroundWake::Tick);
    if let ForegroundWake::Input(event) = wake {
        dispatch_terminal_event(app, event, terminal.as_deref_mut()).await?;
    }
    paint_foreground_frame(app, terminal, service_application).await
}

pub(super) async fn tick_foreground_frame<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    terminal: Option<&mut Terminal<B>>,
    ticker: &mut tokio::time::Interval,
) -> Result<(), TuiError> {
    let wake = next_foreground_wake(app, ticker).await?;
    render_foreground_wake(app, terminal, wake).await
}

async fn wait_for_idle_event(
    app: &mut TuiApp,
    timeout: Duration,
) -> Result<Option<Event>, TuiError> {
    let Some(events) = app.terminal_events.as_mut() else {
        return Ok(None);
    };
    match tokio::time::timeout(timeout, events.recv()).await {
        Ok(Some(event)) => Ok(Some(event?)),
        Ok(None) => Err(TuiError::Other("terminal event stream closed".into())),
        Err(_) => Ok(None),
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
    app.terminal_events = Some(TerminalEventSource::spawn());
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
            exit_code: app.exit.code(),
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
    // Rendering a large transcript is materially more expensive than polling
    // the event queue. Keep animation/streaming responsive, but avoid
    // rebuilding the entire frame five times a second while the UI is idle.
    const IDLE_REDRAW_INTERVAL: Duration = Duration::from_secs(1);
    let mut frame_dirty = true;
    let mut last_idle_draw = std::time::Instant::now();

    while !app.exit.is_requested() {
        tick_application(app).await?;
        let is_animating = app.busy_state.is_active()
            || app.pending_approved_tool.is_some()
            || app.interactive_terminal.is_some();
        if frame_dirty || is_animating || last_idle_draw.elapsed() >= IDLE_REDRAW_INTERVAL {
            terminal.draw(|f| app.draw(f))?;
            frame_dirty = false;
            last_idle_draw = std::time::Instant::now();
        }

        // Drain queued user prompt with streaming redraws (YOU paints before first token)
        if app.pending_turn.has_prompt() {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        if app.pending_turn.continue_requested() {
            app.drain_pending_prompt(Some(terminal)).await?;
            continue;
        }
        if app.pending_interaction.has_hitl_decision() {
            app.drain_pending_hitl(Some(terminal)).await?;
            continue;
        }
        if app.pending_interaction.has_question_submit() {
            app.drain_pending_question(Some(terminal)).await?;
            continue;
        }
        if app.pending_interaction.context_reset_pending() {
            app.drain_pending_context_reset(Some(terminal)).await?;
            continue;
        }
        if app.external_editor.requested {
            app.drain_pending_external_editor(Some(terminal)).await?;
            continue;
        }

        let input_wait = if app.interactive_terminal.is_some() {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(200)
        };
        if let Some(event) = wait_for_idle_event(app, input_wait).await? {
            // Any terminal input changes state directly or through the drained
            // queue; force the next frame rather than waiting for idle cadence.
            frame_dirty = true;
            dispatch_terminal_event(app, event, Some(terminal)).await?;
            drain_events(app, Some(terminal)).await?;
            app.poll_interactive_terminal();
            // Next loop iteration draws once after all input and background polls.
        }
    }
    Ok(())
}
