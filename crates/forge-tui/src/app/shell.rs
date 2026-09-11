//! TUI startup, event loop and terminal lifecycle for [`TuiApp`].
//!
//! Split out of `app.rs` per #19. `run_tui` owns terminal setup and teardown;
//! `run_loop` polls workspace/run state and drains pending work between frames.
//! Methods are moved verbatim.

use super::*;
pub(crate) struct TerminalEventSource {
    rx: tokio::sync::mpsc::UnboundedReceiver<io::Result<Event>>,
    pending: std::collections::VecDeque<io::Result<Event>>,
    ready: std::collections::VecDeque<io::Result<Event>>,
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
            pending: std::collections::VecDeque::new(),
            ready: std::collections::VecDeque::new(),
            stop,
            task: Some(task),
        }
    }

    async fn recv_raw(&mut self) -> Option<io::Result<Event>> {
        if let Some(item) = self.pending.pop_front() {
            return Some(item);
        }
        self.rx.recv().await
    }

    async fn recv_channel(&mut self) -> Option<io::Result<Event>> {
        self.rx.recv().await
    }

    fn try_recv_raw(
        &mut self,
    ) -> Result<io::Result<Event>, tokio::sync::mpsc::error::TryRecvError> {
        if let Some(item) = self.pending.pop_front() {
            return Ok(item);
        }
        self.rx.try_recv()
    }

    fn pop_ready(&mut self) -> Option<io::Result<Event>> {
        self.ready.pop_front()
    }

    fn pop_queued(&mut self) -> Option<(io::Result<Event>, bool)> {
        self.ready
            .pop_front()
            .map(|event| (event, true))
            .or_else(|| self.pending.pop_front().map(|event| (event, false)))
    }

    /// Stash already-read raw events back at the head of the queue, preserving
    /// order, so a lookahead that did not coalesce loses nothing.
    fn push_front_raw(&mut self, item: io::Result<Event>) {
        self.pending.push_front(item);
    }

    /// Release a lookahead as already-classified events. These events must not
    /// be reconsidered as a new paste candidate on the next frame: otherwise a
    /// single typed Enter after a long line would incur one lookahead timeout
    /// per character.
    fn release_ready(&mut self, events: std::collections::VecDeque<Event>) {
        for event in events.into_iter().rev() {
            self.ready.push_front(Ok(event));
        }
    }

    #[cfg(test)]
    fn from_events(events: impl IntoIterator<Item = Event>) -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for event in events {
            tx.send(Ok(event)).unwrap();
        }
        drop(tx);
        Self {
            rx,
            pending: std::collections::VecDeque::new(),
            ready: std::collections::VecDeque::new(),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            task: None,
        }
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
    /// Service every terminal owned by the application, including terminals
    /// moved into saved session views. A saved terminal still has a live PTY;
    /// polling only the selected view lets its bounded reader queue fill and
    /// eventually back up the child process.
    pub(super) fn poll_interactive_terminals(&mut self) -> bool {
        let mut active_changed = false;
        if let Some(terminal) = self.interactive_terminal.as_mut() {
            active_changed = terminal.poll();
            if terminal.take_command_completion().is_some() {
                active_changed = true;
            }
        }
        for state in self.session_view_states.values_mut() {
            if let Some(terminal) = state.interactive_terminal.as_mut() {
                let _ = terminal.poll();
            }
        }
        for state in self.retiring_session_view_states.values_mut() {
            if let Some(terminal) = state.interactive_terminal.as_mut() {
                let _ = terminal.poll();
            }
        }
        active_changed
    }

    pub(super) fn any_interactive_terminal_running(&self) -> bool {
        self.interactive_terminal
            .as_ref()
            .is_some_and(|terminal| terminal.running)
            || self.session_view_states.values().any(|state| {
                state
                    .interactive_terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.running)
            })
            || self.retiring_session_view_states.values().any(|state| {
                state
                    .interactive_terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.running)
            })
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
///
/// Unbracketed multi-line pastes (tmux `paste-buffer`, terminals without
/// DECSET 2004) arrive as plain `Enter` key events, and each one would submit
/// the composer as its own turn. `coalesce_unbracketed_paste` folds a queued
/// burst of printable keys plus `Enter` into a single `Event::Paste` so the
/// whole payload lands as one pending composer message.
const MAX_EVENTS_PER_FRAME: usize = 32;

/// A paste can cross the reader thread's poll boundary. Once a plain Enter
/// has arrived, give the rest of that burst a short window to arrive before
/// deciding that it was an ordinary submit. This is long enough to span the
/// 20ms terminal-reader poll while keeping a deliberate Enter responsive.
const UNBRACKETED_PASTE_LOOKAHEAD: Duration = Duration::from_millis(60);

/// Bound an unbracketed lookahead even if a producer continuously sends
/// printable key events without another newline.
const MAX_UNBRACKETED_PASTE_EVENTS: usize = 64 * 1024;

/// Minimum queued lines that mark an `Enter` burst as an unbracketed paste
/// rather than deliberate submits. A lone `Enter` (or one newline between two
/// typed fragments) keeps submitting; two or more queued `Enter` presses in
/// the same drain mean the newlines arrived as fast as the reader thread
/// could forward them, which typing cannot produce.
const UNBRACKETED_PASTE_MIN_NEWLINES: usize = 2;

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
        let next = coalesce_unbracketed_paste(&mut app.test_events);
        #[cfg(not(test))]
        let next = coalesce_channel_paste(app).await;
        let Some(next) = next else {
            break;
        };
        #[cfg(not(test))]
        let next = next?;
        dispatch_terminal_event(app, next, terminal.as_deref_mut()).await?;
    }
    Ok(())
}

/// Pop the next production terminal event, folding an already-queued burst of
/// printable key events plus plain `Enter` presses into a single `Event::Paste`
/// (see `coalesce_unbracketed_paste`). A plain Enter is held briefly so a
/// paste that is still arriving cannot submit its first line before the
/// reader has delivered the next one.
#[cfg(not(test))]
async fn coalesce_channel_paste(app: &mut TuiApp) -> Option<io::Result<Event>> {
    let events = app.terminal_events.as_mut()?;
    coalesce_source_paste(events, None).await
}

#[cfg(not(test))]
async fn coalesce_channel_paste_with_initial(
    app: &mut TuiApp,
    initial: Option<Event>,
) -> Option<io::Result<Event>> {
    let Some(events) = app.terminal_events.as_mut() else {
        return initial.map(Ok);
    };
    coalesce_source_paste(events, initial).await
}

async fn coalesce_source_paste(
    events: &mut TerminalEventSource,
    initial: Option<Event>,
) -> Option<io::Result<Event>> {
    use tokio::sync::mpsc::error::TryRecvError;

    let mut queued: std::collections::VecDeque<Event> = std::collections::VecDeque::new();
    if let Some(event) = initial {
        if !is_unbracketed_key_event(&event) {
            return Some(Ok(event));
        }
        queued.push_back(event);
    } else if let Some(item) = events.pop_ready() {
        return Some(item);
    }

    let mut newlines = queued.iter().filter(|event| is_plain_enter(event)).count();
    let mut closed = false;
    loop {
        let result = events.try_recv_raw();
        match result {
            Ok(Ok(event)) if is_unbracketed_key_event(&event) => {
                newlines += usize::from(is_plain_enter(&event));
                queued.push_back(event);
            }
            Ok(Ok(event)) if queued.is_empty() => return Some(Ok(event)),
            Ok(Ok(event)) => {
                events.push_front_raw(Ok(event));
                break;
            }
            Ok(Err(error)) => {
                for event in queued.into_iter().rev() {
                    events.push_front_raw(Ok(event));
                }
                return Some(Err(error));
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                closed = true;
                break;
            }
        }

        // Keep normal key bursts bounded for redraw fairness. Once a newline
        // is present, continue looking through the whole candidate so a
        // second newline beyond the old 32-event boundary still makes the
        // entire unbracketed paste atomic.
        if newlines >= UNBRACKETED_PASTE_MIN_NEWLINES
            || (newlines == 0 && queued.len() >= MAX_EVENTS_PER_FRAME)
            || queued.len() >= MAX_UNBRACKETED_PASTE_EVENTS
        {
            break;
        }
    }

    // A plain Enter is the point at which a queued key burst becomes
    // ambiguous. Wait for a short quiet window for the next line. The source
    // is still the same single reader, so events collected here retain their
    // original order.
    if newlines > 0 && newlines < UNBRACKETED_PASTE_MIN_NEWLINES && !closed {
        let deadline = tokio::time::Instant::now() + UNBRACKETED_PASTE_LOOKAHEAD;
        while newlines < UNBRACKETED_PASTE_MIN_NEWLINES
            && queued.len() < MAX_UNBRACKETED_PASTE_EVENTS
        {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let result = tokio::time::timeout(remaining, events.recv_raw()).await;
            match result {
                Ok(Some(Ok(event))) if is_unbracketed_key_event(&event) => {
                    newlines += usize::from(is_plain_enter(&event));
                    queued.push_back(event);
                }
                Ok(Some(Ok(event))) => {
                    events.push_front_raw(Ok(event));
                    break;
                }
                Ok(Some(Err(error))) => {
                    for event in queued.into_iter().rev() {
                        events.push_front_raw(Ok(event));
                    }
                    return Some(Err(error));
                }
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_) => break,
            }
        }
    }

    if queued.is_empty() {
        return closed.then(|| {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "terminal event stream closed",
            ))
        });
    }

    if newlines >= UNBRACKETED_PASTE_MIN_NEWLINES {
        let coalesced = coalesce_unbracketed_paste(&mut queued);
        for event in queued.into_iter().rev() {
            events.push_front_raw(Ok(event));
        }
        return coalesced.map(Ok);
    }

    // No second newline arrived. Release all inspected events as an ordinary
    // key sequence in one classified batch; subsequent calls must not repeat
    // the lookahead timeout for every character in the line.
    events.release_ready(queued);
    events.ready.pop_front()
}

fn is_plain_enter(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Enter
                && key.modifiers.is_empty()
    )
}

fn is_unbracketed_key_event(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind != KeyEventKind::Press {
        return false;
    }
    let non_shift_modifiers = key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE);
    matches!(key.code, KeyCode::Enter if key.modifiers.is_empty())
        || matches!(key.code, KeyCode::Char(c) if !c.is_control() && non_shift_modifiers.is_empty())
}

/// Fold a queued burst of printable key events plus plain `Enter` presses into
/// a single `Event::Paste`, so a multi-line paste that arrives without
/// bracketed-paste framing lands as one pending composer message instead of N
/// separate submits. Only `Press` key events participate: plain `Enter`, and
/// printable characters with no modifiers beyond `Shift` (shifted capitals
/// arrive with `Shift` held and still insert as text). Anything else
/// (modified chords, `Shift+Enter`, mouse, resize, a real bracketed `Paste`)
/// stops the burst and is returned untouched on the next drain.
fn coalesce_unbracketed_paste(queued: &mut std::collections::VecDeque<Event>) -> Option<Event> {
    let mut end = 0usize;
    let mut newlines = 0usize;
    for event in queued.iter() {
        let Event::Key(key) = event else {
            break;
        };
        if key.kind != KeyEventKind::Press {
            break;
        }
        // Mirror `printable_chat_char`: shifted capitals/symbols still
        // insert as text, so they belong to the burst. Plain `Enter`
        // submits, so only modifier-free `Enter` counts as a pasted
        // newline — `Shift+Enter` inserts its own newline.
        let non_shift_modifiers = key.modifiers & !(KeyModifiers::SHIFT | KeyModifiers::NONE);
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                newlines += 1;
                end += 1;
            }
            KeyCode::Char(c) if !c.is_control() && non_shift_modifiers.is_empty() => {
                end += 1;
            }
            _ => break,
        }
    }
    if newlines < UNBRACKETED_PASTE_MIN_NEWLINES || end < 2 {
        return queued.pop_front();
    }
    let mut pasted = String::new();
    for _ in 0..end {
        match queued.pop_front() {
            Some(Event::Key(key)) => match key.code {
                KeyCode::Enter => pasted.push('\n'),
                KeyCode::Char(c) => pasted.push(c),
                _ => {}
            },
            _ => break,
        }
    }
    Some(Event::Paste(pasted))
}

/// Wait until terminal input actually arrives or a time-based UI service is due.
/// Stream/model events use their own wake source and never wait for this ticker.
pub(super) async fn next_foreground_wake(
    app: &mut TuiApp,
    ticker: &mut tokio::time::Interval,
) -> Result<ForegroundWake, TuiError> {
    #[cfg(test)]
    {
        if let Some(event) = coalesce_unbracketed_paste(&mut app.test_events) {
            return Ok(ForegroundWake::Input(event));
        }
        ticker.tick().await;
        Ok(ForegroundWake::Tick)
    }

    #[cfg(not(test))]
    {
        if app.terminal_events.is_none() {
            ticker.tick().await;
            return Ok(ForegroundWake::Tick);
        }

        // Consume our classified/raw lookahead queues before entering the
        // ticker select. Popping from a queue inside a cancellable select
        // future would make a simultaneous tick able to lose that event.
        if let Some((event, already_classified)) = app
            .terminal_events
            .as_mut()
            .and_then(TerminalEventSource::pop_queued)
        {
            return foreground_input(app, event, already_classified).await;
        }

        tokio::select! {
            event = recv_foreground_channel(app) => {
                let event = event
                    .ok_or_else(|| TuiError::Other("terminal event stream closed".into()))?;
                foreground_input(app, event, false).await
            }
            _ = ticker.tick() => Ok(ForegroundWake::Tick),
        }
    }
}

#[cfg(not(test))]
async fn recv_foreground_channel(app: &mut TuiApp) -> Option<io::Result<Event>> {
    let events = app.terminal_events.as_mut()?;
    events.recv_channel().await
}

#[cfg(not(test))]
async fn foreground_input(
    app: &mut TuiApp,
    event: io::Result<Event>,
    already_classified: bool,
) -> Result<ForegroundWake, TuiError> {
    let event = event?;
    if already_classified {
        return Ok(ForegroundWake::Input(event));
    }
    let event = coalesce_channel_paste_with_initial(app, Some(event))
        .await
        .ok_or_else(|| TuiError::Other("terminal event stream closed".into()))??;
    Ok(ForegroundWake::Input(event))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(event::KeyEvent::new(code, KeyModifiers::NONE))
    }

    async fn collect_events(events: Vec<Event>) -> Vec<Event> {
        let mut source = TerminalEventSource::from_events(events);
        let mut result = Vec::new();
        loop {
            match coalesce_source_paste(&mut source, None).await {
                Some(Ok(event)) => result.push(event),
                Some(Err(error)) if error.kind() == io::ErrorKind::BrokenPipe => break,
                Some(Err(error)) => panic!("unexpected terminal error: {error}"),
                None => break,
            }
        }
        result
    }

    #[tokio::test]
    async fn production_event_buffer_keeps_a_large_unbracketed_paste_atomic() {
        let payload = (0..160)
            .map(|line| format!("line {line}: {}\n", "assessment ".repeat(2)))
            .collect::<String>();
        assert!(payload.len() > 2_000);

        let events = payload.chars().map(|character| match character {
            '\n' => key(KeyCode::Enter),
            character => key(KeyCode::Char(character)),
        });
        let output = collect_events(events.collect()).await;
        let mut reconstructed = String::new();
        for event in output {
            match event {
                Event::Paste(text) => reconstructed.push_str(&text),
                Event::Key(key) => match key.code {
                    KeyCode::Char(character) => reconstructed.push(character),
                    _ => panic!("unbracketed paste leaked a control key: {key:?}"),
                },
                other => panic!("unexpected event in paste: {other:?}"),
            }
        }
        assert_eq!(reconstructed, payload);
    }

    #[tokio::test]
    async fn production_event_buffer_still_releases_a_single_typed_enter() {
        let mut events = "ordinary line"
            .chars()
            .map(|character| key(KeyCode::Char(character)))
            .collect::<Vec<_>>();
        events.push(key(KeyCode::Enter));
        let output = collect_events(events).await;

        assert!(output.iter().any(|event| {
            matches!(
                event,
                Event::Key(key) if key.code == KeyCode::Enter
            )
        }));
        assert!(!output.iter().any(|event| matches!(event, Event::Paste(_))));
    }

    #[tokio::test]
    async fn production_event_buffer_handles_a_foreground_wake_at_the_first_enter() {
        let mut source = TerminalEventSource::from_events("second line\nthird line\n".chars().map(
            |character| match character {
                '\n' => key(KeyCode::Enter),
                character => key(KeyCode::Char(character)),
            },
        ));

        let first = coalesce_source_paste(&mut source, Some(key(KeyCode::Enter)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first,
            Event::Paste("\nsecond line\n".into()),
            "a foreground wake must not dispatch the first pasted Enter"
        );
    }

    #[tokio::test]
    async fn production_event_buffer_waits_across_a_reader_boundary() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut source = TerminalEventSource {
            rx,
            pending: std::collections::VecDeque::new(),
            ready: std::collections::VecDeque::new(),
            stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            task: None,
        };
        for character in "first line\n".chars() {
            tx.send(Ok(match character {
                '\n' => key(KeyCode::Enter),
                character => key(KeyCode::Char(character)),
            }))
            .unwrap();
        }
        let sender_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            for character in "second line\n".chars() {
                tx.send(Ok(match character {
                    '\n' => key(KeyCode::Enter),
                    character => key(KeyCode::Char(character)),
                }))
                .unwrap();
            }
        });

        let event = coalesce_source_paste(&mut source, None)
            .await
            .unwrap()
            .unwrap();
        sender_task.await.unwrap();
        assert_eq!(event, Event::Paste("first line\nsecond line\n".into()));
    }
}

/// Advance every non-blocking service owned by the TUI application.
///
/// A foreground model turn temporarily drives its own model/tool state machine,
/// but it must not replace the application loop while it waits. Keeping this
/// tick in one place makes foreground waits and the idle loop service the same
/// file watcher, background tasks, approvals, connection state and transient
/// chrome. Rendering and terminal input stay with the single Ratatui owner.
pub(super) async fn tick_application(app: &mut TuiApp) -> Result<bool, TuiError> {
    app.poll_supervisor_events();
    app.flush_done_pending().await;
    app.poll_file_changes();
    let terminal_changed = app.poll_interactive_terminals();
    if let Some(editor) = app.editor_session.as_mut() {
        editor.refresh_pending_highlights();
    }
    app.tick_render_state();
    app.warm_catalog_once_connected();
    app.poll_catalog_refresh();
    app.poll_background_tasks().await?;
    app.poll_approved_hitl().await?;
    app.tick_toast();
    app.tick_feedback();
    app.drain_auto_hitl().await?;
    // Newly arrived approvals claim focus + scroll-into-view once.
    app.sync_approval_focus();
    app.sync_composer_placeholder();
    app.sync_question_focus();
    // Grok-style device-code: poll token endpoint while overlay is open.
    app.poll_oauth_tick();
    Ok(terminal_changed)
}

/// Run one complete TUI tick while foreground work is in flight, then consume
/// terminal input and paint the resulting state.
pub(super) async fn paint_foreground_frame<B: ratatui::backend::Backend>(
    app: &mut TuiApp,
    mut terminal: Option<&mut Terminal<B>>,
    service_application: bool,
) -> Result<(), TuiError> {
    if service_application {
        let _ = tick_application(app).await?;
    }
    // Deltas arrive irregularly and each one paints, so the reveal has to
    // advance here too — otherwise a burst would sit still until the next
    // 100ms tick and the smoothing would show as stutter instead.
    app.stream.advance_reveal(Instant::now());
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
) -> Result<Option<(Event, bool)>, TuiError> {
    let Some(events) = app.terminal_events.as_mut() else {
        return Ok(None);
    };
    if let Some((event, already_classified)) = events.pop_queued() {
        return Ok(Some((event?, already_classified)));
    }
    match tokio::time::timeout(timeout, events.recv_channel()).await {
        Ok(Some(event)) => Ok(Some((event?, false))),
        Ok(None) => Err(TuiError::Other("terminal event stream closed".into())),
        Err(_) => Ok(None),
    }
}

/// Extra launch flags for first-install / new-project / returning.
#[derive(Clone, Default)]
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
    let app =
        TuiApp::new_with_startup_resume_picker(session, runtime, launch.startup_items.clone());
    run_tui_app_inner(app, launch).await
}

pub async fn run_tui_supervised(
    initial: SessionRuntimeSnapshot,
    runtime: TuiRuntimeConfig,
    handle: SupervisorHandle,
    launch: TuiLaunch,
) -> Result<ExitSummary, TuiError> {
    let app = TuiApp::new_supervised(initial, runtime, handle.clone());
    handle
        .command(forge_session::SupervisorCommand::Refresh)
        .await
        .map_err(|error| TuiError::Other(error.to_string()))?;
    run_tui_app_inner(app, launch).await
}

async fn run_tui_app_inner(mut app: TuiApp, launch: TuiLaunch) -> Result<ExitSummary, TuiError> {
    enable_raw_mode()?;
    // Ensure the terminal is restored on panic, returned errors and normal exit.
    let guard = TerminalGuard::install();
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

    // Repository mode installs its supervisor state in `new_supervised`; the
    // direct launcher deliberately has no runtime-ownership escape hatch.
    app.terminal_events = Some(TerminalEventSource::spawn());
    app.onboarding_connect = launch.onboarding_connect;
    if launch.ready_placeholder {
        app.input.hint = crate::app::types::COMPOSER_OPENER.into();
    }
    if app.overlay.is_none() && launch.onboarding_connect && !app.is_provider_connected() {
        app.open_connect_picker();
        app.set_feedback(FeedbackSeverity::Info, "Connect a provider · Esc quits");
    }
    let result = run_loop(&mut terminal, &mut app).await;

    app.persist_selection();

    if let Some(session_id) = app.startup_resume.session_id {
        let path = app.selected_journal_dir().join(format!("{session_id}.db"));
        let _ = std::fs::remove_file(path);
    }

    if let Some(supervisor) = app.supervisor.as_ref() {
        supervisor
            .handle
            .command(forge_session::SupervisorCommand::Shutdown)
            .await
            .map_err(|error| TuiError::Other(error.to_string()))?;
        app.poll_supervisor_events();
    }
    let usage = app.selected_token_usage_report();
    let summary = ExitSummary {
        exit_code: app.exit.code(),
        session_id: app.selected_session_id.to_string(),
        token_usage: (usage.api.total_api_tokens() > 0)
            .then(|| format_exit_token_usage(&usage.api)),
    };

    drop(guard);

    result.map(|_| summary)
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
        frame_dirty |= tick_application(app).await?;
        let is_animating = app.busy_state.is_active()
            || app.pending_approved_tool.is_some()
            || app.any_interactive_terminal_running();
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

        let input_wait = if app.any_interactive_terminal_running() {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(200)
        };
        if let Some((event, already_classified)) = wait_for_idle_event(app, input_wait).await? {
            // Any terminal input changes state directly or through the drained
            // queue; force the next frame rather than waiting for idle cadence.
            // The idle wait already consumed the burst head from the channel,
            // so return it to the lookahead before coalescing: otherwise the
            // first Enter would submit a partial line and the rest of the
            // burst would arrive as separate turns.
            frame_dirty = true;
            #[cfg(test)]
            let _ = already_classified;
            #[cfg(not(test))]
            if already_classified {
                dispatch_terminal_event(app, event, Some(terminal)).await?;
            } else if let Some(events) = app.terminal_events.as_mut() {
                events.push_front_raw(Ok(event));
            }
            #[cfg(test)]
            app.test_events.push_front(event);
            drain_events(app, Some(terminal)).await?;
            app.poll_interactive_terminals();
            // Next loop iteration draws once after all input and background polls.
        }
    }
    Ok(())
}
