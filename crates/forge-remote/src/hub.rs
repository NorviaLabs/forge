//! Per-session event logs: how a device sees a session's events in order,
//! and never silently misses one.
//!
//! The hub consumes the backend's events on one task. Each *followed*
//! session gets a log whose entries are numbered 1, 2, 3… without gaps; an
//! entry is serialized once and shared by every subscriber. An event's SSE
//! id is `<epoch>.<seq>`, the epoch being random per hub, so a restarted
//! service is never mistaken for a continuation.
//!
//! * Following a session appends its current state as a `session` event
//!   and streams from there.
//! * Resuming from an id replays what the log still holds after it;
//!   otherwise (another epoch, too old, ahead of the log) a fresh `session`
//!   event marked `resync` is appended and streamed from.
//! * When the backend reports lag, each followed session it names gets a
//!   fresh `session` event marked `resync`.
//! * A subscriber whose queue is full is disconnected, never skipped: its
//!   stream ends after the last event it was given, and it resumes from
//!   there.
//! * A log nobody has followed for [`HubConfig::idle_retention`] is dropped.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use forge_types::SessionId;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::backend::{BackendError, BackendEvent, Interest, RemoteBackend};
use crate::protocol::StreamEvent;

#[derive(Debug, Clone)]
pub struct HubConfig {
    /// Entries a session's log keeps for replay.
    pub max_events: usize,
    /// Serialized bytes a session's log keeps for replay.
    pub max_bytes: usize,
    /// Frames queued for one subscriber before it is disconnected.
    pub subscriber_queue: usize,
    /// How long a log with no subscribers is kept for a reconnect.
    pub idle_retention: Duration,
    /// Logs kept at once; past this the least recently followed idle log
    /// is dropped. Logs someone is watching are never dropped.
    pub max_logs: usize,
    /// Streamed text for a session is held this long and sent as one event,
    /// so a reply does not cost an event per token. Zero sends each delta.
    pub text_batch: Duration,
    /// A held batch is sent once it reaches this many bytes.
    pub text_batch_bytes: usize,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            max_events: 512,
            max_bytes: 1024 * 1024,
            subscriber_queue: 256,
            idle_retention: Duration::from_secs(120),
            max_logs: 8,
            text_batch: Duration::from_millis(50),
            text_batch_bytes: 4096,
        }
    }
}

/// An event's place in a session's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventId {
    pub epoch: String,
    pub seq: u64,
}

impl EventId {
    /// Parse `<epoch>.<seq>`.
    pub fn parse(text: &str) -> Option<Self> {
        let (epoch, seq) = text.split_once('.')?;
        if epoch.is_empty()
            || !epoch.bytes().all(|b| b.is_ascii_alphanumeric())
            || seq.is_empty()
            || !seq.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            epoch: epoch.to_string(),
            seq: seq.parse().ok()?,
        })
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.epoch, self.seq)
    }
}

/// One logged event, ready to send.
#[derive(Debug, Clone)]
pub struct Frame {
    pub id: EventId,
    /// The SSE event name: `session`, `text`, `notice` or `gone`.
    pub event: &'static str,
    /// The event as JSON.
    pub data: Arc<str>,
}

/// A size snapshot of one session's log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogStats {
    pub events: usize,
    pub bytes: usize,
    pub head: u64,
    pub subscribers: usize,
}

/// One follower's frames, in log order. `None` from [`Self::recv`] means
/// the stream is over: the session went away, the hub stopped, or this
/// subscriber fell too far behind and must resume.
pub struct Subscription {
    session_id: SessionId,
    frames: mpsc::Receiver<Frame>,
}

impl Subscription {
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub async fn recv(&mut self) -> Option<Frame> {
        self.frames.recv().await
    }
}

pub struct Hub {
    epoch: String,
    interest: Interest,
    commands: mpsc::Sender<Command>,
    cancel: CancellationToken,
}

enum Command {
    Follow {
        session_id: SessionId,
        resume: Option<EventId>,
        reply: oneshot::Sender<Result<Subscription, BackendError>>,
    },
    Logged {
        reply: oneshot::Sender<Vec<SessionId>>,
    },
    Stats {
        session_id: SessionId,
        reply: oneshot::Sender<Option<LogStats>>,
    },
}

impl Hub {
    /// Subscribe to `backend` and start the hub's task on the current
    /// runtime.
    pub fn start(backend: Arc<dyn RemoteBackend>, config: HubConfig) -> Self {
        let interest = Interest::default();
        let events = backend.subscribe(interest.clone());
        let (commands, command_rx) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let epoch = random_epoch();
        let task = HubTask {
            backend,
            config,
            epoch: epoch.clone(),
            interest: interest.clone(),
            logs: HashMap::new(),
            batches: HashMap::new(),
        };
        tokio::spawn(task.run(events, command_rx, cancel.clone()));
        Self {
            epoch,
            interest,
            commands,
            cancel,
        }
    }

    pub fn epoch(&self) -> String {
        self.epoch.clone()
    }

    /// The sessions being logged, shared with the backend.
    pub fn interest(&self) -> Interest {
        self.interest.clone()
    }

    /// Follow `session_id`, from `resume` when the device has seen events
    /// before.
    pub async fn follow(
        &self,
        session_id: SessionId,
        resume: Option<EventId>,
    ) -> Result<Subscription, BackendError> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Follow {
                session_id,
                resume,
                reply,
            })
            .await
            .map_err(|_| stopped())?;
        answer.await.map_err(|_| stopped())?
    }

    /// The sessions with a log.
    pub async fn logged_sessions(&self) -> Vec<SessionId> {
        let (reply, answer) = oneshot::channel();
        if self.commands.send(Command::Logged { reply }).await.is_err() {
            return Vec::new();
        }
        answer.await.unwrap_or_default()
    }

    /// Sizes of a session's log, when it has one.
    pub async fn stats(&self, session_id: SessionId) -> Option<LogStats> {
        let (reply, answer) = oneshot::channel();
        self.commands
            .send(Command::Stats { session_id, reply })
            .await
            .ok()?;
        answer.await.ok()?
    }

    /// End every stream and stop the task.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Wait until `due`, or forever when nothing is held.
async fn sleep_until_due(due: Option<Instant>) {
    match due {
        Some(due) => tokio::time::sleep_until(due).await,
        None => std::future::pending().await,
    }
}

fn stopped() -> BackendError {
    BackendError::Unavailable("remote control has stopped".into())
}

fn random_epoch() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the operating system's random source");
    hex::encode(bytes)
}

/// One followed session.
struct Log {
    entries: VecDeque<Frame>,
    bytes: usize,
    head: u64,
    subscribers: Vec<mpsc::Sender<Frame>>,
    /// When the last subscriber was seen gone.
    idle_since: Option<Instant>,
    /// When someone last started following it.
    last_followed: Instant,
}

impl Log {
    fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            head: 0,
            subscribers: Vec::new(),
            idle_since: None,
            last_followed: Instant::now(),
        }
    }

    /// The oldest seq the log can still replay from (exclusive bound).
    fn replayable_after(&self) -> u64 {
        self.entries
            .front()
            .map_or(self.head, |first| first.id.seq - 1)
    }
}

struct HubTask {
    backend: Arc<dyn RemoteBackend>,
    config: HubConfig,
    epoch: String,
    interest: Interest,
    logs: HashMap<SessionId, Log>,
    /// Streamed text not yet logged, per session.
    batches: HashMap<SessionId, TextBatch>,
}

/// Text held back to be sent as one event.
struct TextBatch {
    text: String,
    /// When the batch goes out at the latest.
    due: Instant,
}

impl HubTask {
    async fn run(
        mut self,
        mut events: mpsc::Receiver<BackendEvent>,
        mut commands: mpsc::Receiver<Command>,
        cancel: CancellationToken,
    ) {
        let period = (self.config.idle_retention / 4).max(Duration::from_millis(10));
        let mut sweep = tokio::time::interval(period);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let due = self.batches.values().map(|batch| batch.due).min();
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = sleep_until_due(due) => self.flush_due(),
                command = commands.recv() => match command {
                    Some(command) => self.command(command).await,
                    None => break,
                },
                event = events.recv() => match event {
                    Some(event) => self.event(event).await,
                    // The backend went away: nothing more will happen.
                    None => break,
                },
                _ = sweep.tick() => self.sweep(),
            }
        }
        for session_id in self.logs.keys() {
            self.interest.remove(*session_id);
        }
        // Dropping the logs drops every subscriber's sender: their
        // streams end.
    }

    async fn command(&mut self, command: Command) {
        match command {
            Command::Follow {
                session_id,
                resume,
                reply,
            } => {
                let result = self.follow(session_id, resume).await;
                let _ = reply.send(result);
            }
            Command::Logged { reply } => {
                let mut sessions: Vec<SessionId> = self.logs.keys().copied().collect();
                sessions.sort();
                let _ = reply.send(sessions);
            }
            Command::Stats { session_id, reply } => {
                let _ = reply.send(self.logs.get(&session_id).map(|log| LogStats {
                    events: log.entries.len(),
                    bytes: log.bytes,
                    head: log.head,
                    subscribers: log.subscribers.iter().filter(|s| !s.is_closed()).count(),
                }));
            }
        }
    }

    async fn follow(
        &mut self,
        session_id: SessionId,
        resume: Option<EventId>,
    ) -> Result<Subscription, BackendError> {
        // Held text belongs before anything that follows it.
        self.flush(session_id);
        // Replay when the log still holds everything after `resume`.
        if let Some(from) = &resume {
            if let Some(log) = self.logs.get_mut(&session_id) {
                if from.epoch == self.epoch
                    && from.seq <= log.head
                    && from.seq >= log.replayable_after()
                {
                    let missed: Vec<Frame> = log
                        .entries
                        .iter()
                        .filter(|frame| frame.id.seq > from.seq)
                        .cloned()
                        .collect();
                    let (sender, frames) =
                        mpsc::channel(self.config.subscriber_queue + missed.len().max(1));
                    for frame in missed {
                        // Sized to fit: this cannot fail.
                        let _ = sender.try_send(frame);
                    }
                    log.subscribers.push(sender);
                    log.idle_since = None;
                    return Ok(Subscription { session_id, frames });
                }
            }
        }
        // Otherwise start from the session as it stands.
        let state = self.backend.session_state(session_id).await?;
        if !self.logs.contains_key(&session_id) {
            self.make_room();
        }
        self.interest.insert(session_id);
        self.logs
            .entry(session_id)
            .or_insert_with(Log::new)
            .last_followed = Instant::now();
        let frame = self.append(
            session_id,
            StreamEvent::Session {
                state: Box::new(state),
                resync: resume.is_some(),
            },
        );
        let (sender, frames) = mpsc::channel(self.config.subscriber_queue + 1);
        if let Some(frame) = frame {
            let _ = sender.try_send(frame);
        }
        if let Some(log) = self.logs.get_mut(&session_id) {
            log.subscribers.push(sender);
            log.idle_since = None;
        }
        Ok(Subscription { session_id, frames })
    }

    async fn event(&mut self, event: BackendEvent) {
        // Any other event for a session sends its held text first, so the
        // log keeps the order Forge produced.
        match &event {
            BackendEvent::Text { .. } => {}
            BackendEvent::Updated(state) => self.flush(state.session_id),
            BackendEvent::Notice { session_id, .. } | BackendEvent::Gone(session_id) => {
                self.flush(*session_id)
            }
            BackendEvent::Lagged { sessions } => {
                for session_id in sessions {
                    self.flush(*session_id);
                }
            }
        }
        match event {
            BackendEvent::Updated(state) => {
                let session_id = state.session_id;
                if self.logs.contains_key(&session_id) {
                    self.append(
                        session_id,
                        StreamEvent::Session {
                            state,
                            resync: false,
                        },
                    );
                }
            }
            BackendEvent::Text { session_id, delta } => {
                if self.logs.contains_key(&session_id) {
                    self.batch(session_id, delta);
                }
            }
            BackendEvent::Notice {
                session_id,
                kind,
                message,
            } => {
                if self.logs.contains_key(&session_id) {
                    self.append(
                        session_id,
                        StreamEvent::Notice {
                            session_id,
                            kind,
                            message,
                        },
                    );
                }
            }
            BackendEvent::Gone(session_id) => self.gone(session_id),
            BackendEvent::Lagged { sessions } => {
                for session_id in sessions {
                    if !self.logs.contains_key(&session_id) {
                        continue;
                    }
                    match self.backend.session_state(session_id).await {
                        Ok(state) => {
                            self.append(
                                session_id,
                                StreamEvent::Session {
                                    state: Box::new(state),
                                    resync: true,
                                },
                            );
                        }
                        Err(BackendError::UnknownSession) => self.gone(session_id),
                        // Unreadable for now: the next update replaces it.
                        Err(_) => {}
                    }
                }
            }
        }
    }

    /// Hold `delta` with the session's other unsent text; send the batch
    /// when it is full (or at once when batching is off).
    fn batch(&mut self, session_id: SessionId, delta: String) {
        if self.config.text_batch.is_zero() {
            self.append(session_id, StreamEvent::Text { session_id, delta });
            return;
        }
        let due = Instant::now() + self.config.text_batch;
        let batch = self.batches.entry(session_id).or_insert_with(|| TextBatch {
            text: String::new(),
            due,
        });
        batch.text.push_str(&delta);
        if batch.text.len() >= self.config.text_batch_bytes {
            self.flush(session_id);
        }
    }

    /// Log a session's held text, if any.
    fn flush(&mut self, session_id: SessionId) {
        if let Some(batch) = self.batches.remove(&session_id) {
            if !batch.text.is_empty() && self.logs.contains_key(&session_id) {
                self.append(
                    session_id,
                    StreamEvent::Text {
                        session_id,
                        delta: batch.text,
                    },
                );
            }
        }
    }

    /// Log every batch whose time is up.
    fn flush_due(&mut self) {
        let now = Instant::now();
        let due: Vec<SessionId> = self
            .batches
            .iter()
            .filter(|(_, batch)| batch.due <= now)
            .map(|(session_id, _)| *session_id)
            .collect();
        for session_id in due {
            self.flush(session_id);
        }
    }

    fn gone(&mut self, session_id: SessionId) {
        self.batches.remove(&session_id);
        if self.logs.contains_key(&session_id) {
            self.append(session_id, StreamEvent::Gone { session_id });
            // Dropping the log drops the senders; each stream ends after
            // the `gone` frame already queued.
            self.logs.remove(&session_id);
            self.interest.remove(session_id);
        }
    }

    /// Number `event`, keep it, and hand it to every subscriber. A
    /// subscriber that cannot take it is dropped, never skipped.
    fn append(&mut self, session_id: SessionId, event: StreamEvent) -> Option<Frame> {
        let log = self.logs.get_mut(&session_id)?;
        let data: Arc<str> = match serde_json::to_string(&event) {
            Ok(json) => json.into(),
            Err(error) => {
                tracing::warn!(%error, "remote: an event could not be serialized");
                return None;
            }
        };
        log.head += 1;
        let frame = Frame {
            id: EventId {
                epoch: self.epoch.clone(),
                seq: log.head,
            },
            event: event.name(),
            data,
        };
        log.subscribers
            .retain(|subscriber| subscriber.try_send(frame.clone()).is_ok());
        log.bytes += frame.data.len();
        log.entries.push_back(frame.clone());
        while log.entries.len() > 1
            && (log.entries.len() > self.config.max_events || log.bytes > self.config.max_bytes)
        {
            if let Some(old) = log.entries.pop_front() {
                log.bytes -= old.data.len();
            }
        }
        Some(frame)
    }

    /// Drop the least recently followed idle logs until a new one fits.
    /// Watched logs are never dropped; the stream limit bounds those.
    fn make_room(&mut self) {
        for log in self.logs.values_mut() {
            log.subscribers.retain(|subscriber| !subscriber.is_closed());
        }
        while self.logs.len() >= self.config.max_logs {
            let oldest_idle = self
                .logs
                .iter()
                .filter(|(_, log)| log.subscribers.is_empty())
                .min_by_key(|(_, log)| log.last_followed)
                .map(|(session_id, _)| *session_id);
            let Some(session_id) = oldest_idle else {
                break;
            };
            self.logs.remove(&session_id);
            self.batches.remove(&session_id);
            self.interest.remove(session_id);
        }
    }

    /// Forget closed subscribers, and logs nobody has followed for a while.
    fn sweep(&mut self) {
        let now = Instant::now();
        let retention = self.config.idle_retention;
        let mut expired = Vec::new();
        for (session_id, log) in &mut self.logs {
            log.subscribers.retain(|subscriber| !subscriber.is_closed());
            if !log.subscribers.is_empty() {
                log.idle_since = None;
                continue;
            }
            let since = *log.idle_since.get_or_insert(now);
            if now.duration_since(since) >= retention {
                expired.push(*session_id);
            }
        }
        for session_id in expired {
            self.logs.remove(&session_id);
            self.interest.remove(session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{session, FakeBackend};

    fn id() -> SessionId {
        SessionId::new_v4()
    }

    /// One log entry per text delta, for tests that count entries.
    fn unbatched() -> HubConfig {
        HubConfig {
            text_batch: Duration::ZERO,
            ..HubConfig::default()
        }
    }

    async fn hub_with(config: HubConfig, sessions: &[SessionId]) -> (Arc<FakeBackend>, Hub) {
        let backend = FakeBackend::new();
        for (n, session_id) in sessions.iter().enumerate() {
            backend.put(session(*session_id, &format!("session {n}")));
        }
        let hub = Hub::start(backend.clone(), config);
        backend.wait_subscribed().await;
        (backend, hub)
    }

    fn json(frame: &Frame) -> serde_json::Value {
        serde_json::from_str(&frame.data).unwrap()
    }

    async fn next(sub: &mut Subscription) -> Frame {
        tokio::time::timeout(Duration::from_secs(5), sub.recv())
            .await
            .expect("a frame arrives")
            .expect("the stream is open")
    }

    async fn ended(sub: &mut Subscription) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(5), sub.recv()).await,
            Ok(None)
        )
    }

    /// Emit `n` text deltas and wait until the log holds them.
    async fn texts(backend: &FakeBackend, hub: &Hub, session_id: SessionId, n: usize, head: u64) {
        for i in 0..n {
            assert!(backend.text(session_id, &format!("t{i}")));
        }
        let target = head + n as u64;
        for _ in 0..500 {
            if hub
                .stats(session_id)
                .await
                .is_some_and(|s| s.head >= target)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the log never reached seq {target}");
    }

    #[test]
    fn event_ids_print_and_parse() {
        let id = EventId {
            epoch: "3fa291c0d8e44b27".into(),
            seq: 42,
        };
        assert_eq!(id.to_string(), "3fa291c0d8e44b27.42");
        assert_eq!(EventId::parse("3fa291c0d8e44b27.42"), Some(id));
        for bad in ["", "42", "abc.", ".5", "abc.x", "abc.-1", "a.b.c"] {
            assert_eq!(EventId::parse(bad), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn following_starts_with_the_session_then_numbers_every_event_in_order() {
        let a = id();
        let (backend, hub) = hub_with(unbatched(), &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        let first = next(&mut sub).await;
        assert_eq!((first.event, first.id.seq), ("session", 1));
        assert_eq!(first.id.epoch, hub.epoch());
        assert_eq!(json(&first)["resync"], false);
        assert_eq!(json(&first)["session_id"], a.to_string());

        for i in 0..20 {
            backend.text(a, &format!("t{i}"));
        }
        let mut updated = session(a, "renamed");
        updated.turn_state = "running".into();
        backend.update(updated);
        backend.notice(a, "needs you");
        for i in 0..20 {
            let frame = next(&mut sub).await;
            assert_eq!(frame.id.seq, 2 + i);
            assert_eq!(frame.event, "text");
            assert_eq!(json(&frame)["delta"], format!("t{i}"));
        }
        let update = next(&mut sub).await;
        assert_eq!((update.event, update.id.seq), ("session", 22));
        assert_eq!(json(&update)["label"], "renamed");
        let notice = next(&mut sub).await;
        assert_eq!((notice.event, notice.id.seq), ("notice", 23));
    }

    #[tokio::test]
    async fn following_an_unknown_session_is_an_error() {
        let (_backend, hub) = hub_with(HubConfig::default(), &[]).await;
        assert!(matches!(
            hub.follow(id(), None).await,
            Err(BackendError::UnknownSession)
        ));
        assert!(hub.logged_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn sessions_nobody_follows_are_not_logged() {
        let (a, b) = (id(), id());
        let (backend, hub) = hub_with(unbatched(), &[a, b]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        backend.text(b, "for b");
        backend.text(a, "for a");
        let frame = next(&mut sub).await;
        assert_eq!(json(&frame)["delta"], "for a");
        assert_eq!(frame.id.seq, 2);
        assert_eq!(hub.logged_sessions().await, vec![a]);
        let interest = hub.interest();
        assert!(interest.contains(a) && !interest.contains(b));
        assert!(
            backend.interest().is_some_and(|i| i.contains(a)),
            "the backend is told who is followed"
        );
    }

    #[tokio::test]
    async fn a_second_follower_adds_a_snapshot_that_every_follower_sees_in_order() {
        let a = id();
        let (backend, hub) = hub_with(unbatched(), &[a]).await;
        let mut first = hub.follow(a, None).await.unwrap();
        next(&mut first).await;
        backend.text(a, "x");
        assert_eq!(next(&mut first).await.id.seq, 2);
        let mut second = hub.follow(a, None).await.unwrap();
        let snapshot = next(&mut second).await;
        assert_eq!((snapshot.event, snapshot.id.seq), ("session", 3));
        let seen = next(&mut first).await;
        assert_eq!((seen.event, seen.id.seq), ("session", 3));
    }

    #[tokio::test]
    async fn a_resume_inside_the_log_replays_exactly_what_was_missed() {
        let a = id();
        let (backend, hub) = hub_with(unbatched(), &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        texts(&backend, &hub, a, 4, 1).await;
        let mut last = None;
        for _ in 0..4 {
            last = Some(next(&mut sub).await.id);
        }
        let last = last.unwrap();
        assert_eq!(last.seq, 5);
        drop(sub);
        texts(&backend, &hub, a, 5, 5).await;
        let reads = backend.state_reads();

        let mut resumed = hub.follow(a, Some(last)).await.unwrap();
        for seq in 6..=10 {
            let frame = next(&mut resumed).await;
            assert_eq!((frame.event, frame.id.seq), ("text", seq));
        }
        assert_eq!(backend.state_reads(), reads, "a replay reads no state");
        backend.text(a, "live");
        assert_eq!(next(&mut resumed).await.id.seq, 11);
    }

    #[tokio::test]
    async fn a_resume_at_the_head_continues_with_live_events_only() {
        let a = id();
        let (backend, hub) = hub_with(unbatched(), &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        let head = next(&mut sub).await.id;
        drop(sub);
        let mut resumed = hub.follow(a, Some(head)).await.unwrap();
        backend.text(a, "live");
        let frame = next(&mut resumed).await;
        assert_eq!((frame.event, frame.id.seq), ("text", 2));
    }

    async fn assert_resync(hub: &Hub, a: SessionId, resume: EventId, head: u64) {
        let mut resumed = hub.follow(a, Some(resume.clone())).await.unwrap();
        let frame = next(&mut resumed).await;
        assert_eq!(frame.event, "session", "{resume}");
        assert_eq!(json(&frame)["resync"], true, "{resume}");
        assert_eq!(
            frame.id.seq,
            head + 1,
            "a resync is the next entry, never a gap"
        );
    }

    #[tokio::test]
    async fn a_resume_the_log_cannot_serve_gets_a_resync_snapshot() {
        let a = id();
        let config = HubConfig {
            max_events: 4,
            ..unbatched()
        };
        let (backend, hub) = hub_with(config, &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        let epoch = next(&mut sub).await.id.epoch;
        drop(sub);
        texts(&backend, &hub, a, 10, 1).await;
        // Seq 2 has left a four-entry log.
        assert_resync(
            &hub,
            a,
            EventId {
                epoch: epoch.clone(),
                seq: 2,
            },
            11,
        )
        .await;
        // Another epoch: a restarted service.
        assert_resync(
            &hub,
            a,
            EventId {
                epoch: "0000000000000000".into(),
                seq: 12,
            },
            12,
        )
        .await;
        // Ahead of the log.
        assert_resync(&hub, a, EventId { epoch, seq: 999 }, 13).await;
    }

    #[tokio::test]
    async fn a_lag_resyncs_the_followed_sessions_it_names() {
        let (a, b) = (id(), id());
        let (backend, hub) = hub_with(unbatched(), &[a, b]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        let reads = backend.state_reads();
        backend.put(session(a, "after the lag"));
        backend.emit(BackendEvent::Lagged {
            sessions: vec![a, b],
        });
        let frame = next(&mut sub).await;
        assert_eq!((frame.event, frame.id.seq), ("session", 2));
        assert_eq!(json(&frame)["resync"], true);
        assert_eq!(json(&frame)["label"], "after the lag");
        assert_eq!(
            backend.state_reads(),
            reads + 1,
            "only the followed session is read"
        );
        assert_eq!(hub.logged_sessions().await, vec![a]);
    }

    #[tokio::test]
    async fn a_slow_subscriber_is_disconnected_and_never_skips_an_event() {
        let a = id();
        let config = HubConfig {
            subscriber_queue: 4,
            ..unbatched()
        };
        let (backend, hub) = hub_with(config, &[a]).await;
        let mut slow = hub.follow(a, None).await.unwrap();
        texts(&backend, &hub, a, 50, 1).await;
        let mut seqs = Vec::new();
        while let Ok(Some(frame)) = tokio::time::timeout(Duration::from_secs(5), slow.recv()).await
        {
            seqs.push(frame.id);
        }
        assert!(seqs.len() < 51, "the slow subscriber was cut off");
        let expected: Vec<u64> = (1..=seqs.len() as u64).collect();
        assert_eq!(seqs.iter().map(|id| id.seq).collect::<Vec<_>>(), expected);

        let mut resumed = hub.follow(a, seqs.last().cloned()).await.unwrap();
        let mut next_seq = seqs.len() as u64 + 1;
        while next_seq <= 51 {
            let frame = next(&mut resumed).await;
            assert_eq!(frame.id.seq, next_seq, "the resume continues without a gap");
            next_seq += 1;
        }
    }

    #[tokio::test]
    async fn a_log_keeps_within_its_count_and_byte_limits() {
        let a = id();
        let config = HubConfig {
            max_events: 16,
            max_bytes: 2_000,
            ..unbatched()
        };
        let (backend, hub) = hub_with(config, &[a]).await;
        let _sub = hub.follow(a, None).await.unwrap();
        texts(&backend, &hub, a, 100, 1).await;
        let stats = hub.stats(a).await.unwrap();
        assert!(stats.events <= 16, "{stats:?}");
        assert!(stats.bytes <= 2_000, "{stats:?}");
        assert!(stats.events >= 1, "the newest entry is always kept");
        assert_eq!(stats.head, 101);
    }

    #[tokio::test]
    async fn a_session_that_goes_away_ends_its_streams() {
        let a = id();
        let (backend, hub) = hub_with(unbatched(), &[a]).await;
        let mut one = hub.follow(a, None).await.unwrap();
        next(&mut one).await;
        let mut two = hub.follow(a, None).await.unwrap();
        next(&mut two).await;
        next(&mut one).await;
        backend.remove(a);
        for sub in [&mut one, &mut two] {
            let frame = next(sub).await;
            assert_eq!(frame.event, "gone");
            assert!(ended(sub).await);
        }
        assert!(hub.logged_sessions().await.is_empty());
        assert!(!hub.interest().contains(a));
    }

    #[tokio::test(start_paused = true)]
    async fn a_log_nobody_follows_is_dropped_after_its_retention() {
        let a = id();
        let config = HubConfig {
            idle_retention: Duration::from_secs(120),
            ..HubConfig::default()
        };
        let (_backend, hub) = hub_with(config, &[a]).await;
        let sub = hub.follow(a, None).await.unwrap();
        drop(sub);
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(hub.logged_sessions().await, vec![a], "kept for a reconnect");
        tokio::time::sleep(Duration::from_secs(121)).await;
        assert!(hub.logged_sessions().await.is_empty());
        assert!(!hub.interest().contains(a));
    }

    #[tokio::test]
    async fn a_burst_of_text_arrives_as_few_events_with_every_character() {
        let a = id();
        let (backend, hub) = hub_with(HubConfig::default(), &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        let mut sent = String::new();
        for i in 0..200 {
            let delta = format!("t{i} ");
            sent.push_str(&delta);
            backend.text(a, &delta);
        }
        backend.notice(a, "done");
        let mut received = String::new();
        let mut texts = 0;
        let mut seq = 1;
        loop {
            let frame = next(&mut sub).await;
            seq += 1;
            assert_eq!(frame.id.seq, seq, "still contiguous");
            match frame.event {
                "text" => {
                    texts += 1;
                    received.push_str(json(&frame)["delta"].as_str().unwrap());
                }
                "notice" => break,
                other => panic!("unexpected {other}"),
            }
        }
        assert_eq!(
            received, sent,
            "every character, in order, before the notice"
        );
        assert!(texts <= 20, "a burst is batched: {texts} text events");
    }

    #[tokio::test]
    async fn a_lone_delta_is_not_held_back_for_long() {
        let a = id();
        let (backend, hub) = hub_with(HubConfig::default(), &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        let sent = tokio::time::Instant::now();
        backend.text(a, "hello");
        let frame = next(&mut sub).await;
        assert_eq!(json(&frame)["delta"], "hello");
        assert!(
            sent.elapsed() < Duration::from_millis(500),
            "batching adds at most a moment: {:?}",
            sent.elapsed()
        );
    }

    #[tokio::test]
    async fn a_batch_never_grows_past_its_size_limit() {
        let a = id();
        let config = HubConfig {
            text_batch: Duration::from_secs(60),
            text_batch_bytes: 1000,
            ..HubConfig::default()
        };
        let (backend, hub) = hub_with(config, &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        for _ in 0..30 {
            backend.text(a, &"x".repeat(100));
        }
        let frame = next(&mut sub).await;
        let delta = json(&frame)["delta"].as_str().unwrap().len();
        assert!(
            (1000..1100).contains(&delta),
            "flushed at the size limit: {delta}"
        );
    }

    #[tokio::test]
    async fn only_the_most_recent_idle_logs_are_kept() {
        let sessions: Vec<SessionId> = (0..12).map(|_| id()).collect();
        let config = HubConfig {
            max_logs: 4,
            ..HubConfig::default()
        };
        let (_backend, hub) = hub_with(config, &sessions).await;
        // Two sessions stay watched the whole time.
        let mut watched = Vec::new();
        for session_id in &sessions[..2] {
            let mut sub = hub.follow(*session_id, None).await.unwrap();
            next(&mut sub).await;
            watched.push(sub);
        }
        for session_id in &sessions[2..] {
            let mut sub = hub.follow(*session_id, None).await.unwrap();
            next(&mut sub).await;
        }
        let logged = hub.logged_sessions().await;
        assert!(logged.len() <= 4, "{} logs kept", logged.len());
        for kept in [sessions[0], sessions[1], sessions[11]] {
            assert!(logged.contains(&kept), "watched and newest logs stay");
        }
        assert!(
            !logged.contains(&sessions[2]),
            "the oldest idle log went first"
        );
        assert!(!hub.interest().contains(sessions[2]));
        drop(watched);
    }

    #[tokio::test]
    async fn shutdown_ends_every_stream() {
        let a = id();
        let (_backend, hub) = hub_with(HubConfig::default(), &[a]).await;
        let mut sub = hub.follow(a, None).await.unwrap();
        next(&mut sub).await;
        hub.shutdown();
        assert!(ended(&mut sub).await);
    }
}
