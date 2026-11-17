//! Test support (feature `test-support`): a scriptable backend, and an HTTPS
//! client that pins the service's certificate the way a phone does.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use forge_types::SessionId;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use tokio::sync::mpsc;

use crate::backend::{BackendError, BackendEvent, Interest, RemoteBackend};
use crate::protocol::{
    NoticeKind, RemoteApproval, RemoteDecision, RemoteMessage, SessionListItem, SessionState,
};
use crate::tls::{pinned_client_config, Fingerprint};

/// A decision the fake backend received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    pub session_id: SessionId,
    pub call_id: String,
    pub decision: RemoteDecision,
    pub note: Option<String>,
    pub actor: String,
}

#[derive(Default)]
struct FakeState {
    sessions: Vec<SessionState>,
    prompts: Vec<(SessionId, String)>,
    decisions: Vec<Decided>,
    stops: Vec<SessionId>,
    state_reads: usize,
    fail_prompts: Option<BackendError>,
}

/// A backend whose sessions and events a test sets directly.
#[derive(Default)]
pub struct FakeBackend {
    state: Mutex<FakeState>,
    sender: Mutex<Option<mpsc::Sender<BackendEvent>>>,
    interest: Mutex<Option<Interest>>,
    subscribed: tokio::sync::Notify,
}

impl FakeBackend {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Add or replace a session, without an event.
    pub fn put(&self, state: SessionState) {
        let mut guard = self.state();
        match guard
            .sessions
            .iter_mut()
            .find(|s| s.session_id == state.session_id)
        {
            Some(slot) => *slot = state,
            None => guard.sessions.push(state),
        }
    }

    /// Replace a session and emit `Updated` for it.
    pub fn update(&self, state: SessionState) -> bool {
        self.put(state.clone());
        self.emit(BackendEvent::Updated(Box::new(state)))
    }

    /// Stream a text delta.
    pub fn text(&self, session_id: SessionId, delta: &str) -> bool {
        self.emit(BackendEvent::Text {
            session_id,
            delta: delta.into(),
        })
    }

    pub fn notice(&self, session_id: SessionId, message: &str) -> bool {
        self.emit(BackendEvent::Notice {
            session_id,
            kind: NoticeKind::Attention,
            message: message.into(),
        })
    }

    /// Remove a session and emit `Gone`.
    pub fn remove(&self, session_id: SessionId) -> bool {
        self.state().sessions.retain(|s| s.session_id != session_id);
        self.emit(BackendEvent::Gone(session_id))
    }

    /// Send an event to the subscriber. `false` when there is none yet.
    pub fn emit(&self, event: BackendEvent) -> bool {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        match sender {
            Some(sender) => sender.try_send(event).is_ok(),
            None => false,
        }
    }

    /// Wait until the service has subscribed (it does when it starts).
    pub async fn wait_subscribed(&self) {
        loop {
            let notified = self.subscribed.notified();
            if self
                .sender
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .is_some_and(|s| !s.is_closed())
            {
                return;
            }
            let _ = tokio::time::timeout(Duration::from_millis(50), notified).await;
        }
    }

    /// The interest set the hub handed over.
    pub fn interest(&self) -> Option<Interest> {
        self.interest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn prompts(&self) -> Vec<(SessionId, String)> {
        self.state().prompts.clone()
    }

    pub fn decisions(&self) -> Vec<Decided> {
        self.state().decisions.clone()
    }

    pub fn stops(&self) -> Vec<SessionId> {
        self.state().stops.clone()
    }

    /// How many times a session's state was read.
    pub fn state_reads(&self) -> usize {
        self.state().state_reads
    }

    pub fn fail_prompts(&self, error: Option<BackendError>) {
        self.state().fail_prompts = error;
    }
}

#[async_trait]
impl RemoteBackend for FakeBackend {
    async fn sessions(&self) -> Result<Vec<SessionListItem>, BackendError> {
        Ok(self
            .state()
            .sessions
            .iter()
            .map(|s| SessionListItem {
                session_id: s.session_id,
                project: s.project.clone(),
                label: s.label.clone(),
                turn_state: s.turn_state.clone(),
                needs_approval: s.pending_approval.is_some(),
                runs_on: None,
            })
            .collect())
    }

    async fn session_state(&self, session_id: SessionId) -> Result<SessionState, BackendError> {
        let mut guard = self.state();
        guard.state_reads += 1;
        guard
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned()
            .ok_or(BackendError::UnknownSession)
    }

    /// The fake's `messages` are the whole transcript.
    async fn messages(
        &self,
        session_id: SessionId,
        before: Option<usize>,
        limit: usize,
    ) -> Result<crate::protocol::MessagePage, BackendError> {
        let state = self.session_state(session_id).await?;
        let (message_offset, messages) = crate::protocol::page(&state.messages, before, limit);
        Ok(crate::protocol::MessagePage {
            messages,
            message_offset,
            message_total: state.messages.len(),
        })
    }

    fn subscribe(&self, interest: Interest) -> mpsc::Receiver<BackendEvent> {
        let (sender, receiver) = mpsc::channel(16 * 1024);
        *self.sender.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
        *self.interest.lock().unwrap_or_else(|e| e.into_inner()) = Some(interest);
        self.subscribed.notify_waiters();
        receiver
    }

    async fn submit_prompt(&self, session_id: SessionId, text: String) -> Result<(), BackendError> {
        let mut guard = self.state();
        if !guard.sessions.iter().any(|s| s.session_id == session_id) {
            return Err(BackendError::UnknownSession);
        }
        if let Some(error) = guard.fail_prompts.clone() {
            return Err(error);
        }
        guard.prompts.push((session_id, text));
        Ok(())
    }

    async fn decide(
        &self,
        session_id: SessionId,
        call_id: &str,
        decision: RemoteDecision,
        note: Option<String>,
        actor: &str,
    ) -> Result<(), BackendError> {
        let mut guard = self.state();
        let session = guard
            .sessions
            .iter_mut()
            .find(|s| s.session_id == session_id)
            .ok_or(BackendError::UnknownSession)?;
        let pending = session
            .pending_approval
            .as_ref()
            .filter(|p| p.call_id == call_id)
            .ok_or(BackendError::NotPending)?;
        if !pending.remote_options.contains(&decision) {
            return Err(BackendError::NotAllowed(
                "that decision needs the desktop".into(),
            ));
        }
        session.pending_approval = None;
        guard.decisions.push(Decided {
            session_id,
            call_id: call_id.into(),
            decision,
            note,
            actor: actor.into(),
        });
        Ok(())
    }

    async fn stop(&self, session_id: SessionId) -> Result<(), BackendError> {
        let mut guard = self.state();
        if !guard.sessions.iter().any(|s| s.session_id == session_id) {
            return Err(BackendError::UnknownSession);
        }
        guard.stops.push(session_id);
        Ok(())
    }
}

/// A session with one user message and nothing pending.
pub fn session(session_id: SessionId, label: &str) -> SessionState {
    SessionState {
        session_id,
        project: "project".into(),
        label: label.into(),
        turn_state: "idle".into(),
        messages: vec![RemoteMessage {
            role: "user".into(),
            name: None,
            text: "hello".into(),
            attachments: 0,
        }],
        message_offset: 0,
        message_total: 1,
        pending_approval: None,
        queue: Vec::new(),
        waiting_on: None,
        runs_on: None,
    }
}

/// `state` waiting on approval `call_id`, answerable remotely with `options`.
pub fn with_approval(
    mut state: SessionState,
    call_id: &str,
    options: &[RemoteDecision],
) -> SessionState {
    state.turn_state = "waiting".into();
    state.pending_approval = Some(RemoteApproval {
        call_id: call_id.into(),
        tool: "git".into(),
        arguments: "{\"subcommand\": \"reset\"}".into(),
        reason: "destructive git".into(),
        kind: "governance".into(),
        remote_options: options.to_vec(),
        desktop_required: true,
        runs_on: None,
    });
    state
}

#[derive(Debug)]
pub enum ClientError {
    Connect(std::io::Error),
    Tls(std::io::Error),
    Http(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "connect: {e}"),
            Self::Tls(e) => write!(f, "tls: {e}"),
            Self::Http(e) => write!(f, "http: {e}"),
        }
    }
}

/// A response with its body parsed as JSON (`Null` when empty or not JSON).
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: serde_json::Value,
    pub text: String,
    pub headers: hyper::HeaderMap,
}

type Sender = hyper::client::conn::http1::SendRequest<Full<Bytes>>;

/// An HTTPS client that trusts exactly one certificate.
#[derive(Clone)]
pub struct TestClient {
    pub addr: SocketAddr,
    pin: Fingerprint,
}

impl TestClient {
    pub fn new(addr: SocketAddr, pin: Fingerprint) -> Self {
        Self { addr, pin }
    }

    /// A pinned TLS connection, before any HTTP.
    pub async fn tls(
        &self,
    ) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, ClientError> {
        let tcp = tokio::net::TcpStream::connect(self.addr)
            .await
            .map_err(ClientError::Connect)?;
        let connector = tokio_rustls::TlsConnector::from(pinned_client_config(self.pin));
        let name = rustls::pki_types::ServerName::try_from("forge-remote.invalid")
            .map_err(|e| ClientError::Http(e.to_string()))?;
        connector.connect(name, tcp).await.map_err(ClientError::Tls)
    }

    async fn connect(&self) -> Result<Sender, ClientError> {
        let stream = self.tls().await?;
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(sender)
    }

    pub fn request(
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> hyper::Request<Full<Bytes>> {
        let mut builder = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "forge-remote");
        if let Some(token) = token {
            builder = builder.header(hyper::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        if body.is_some() {
            builder = builder.header(hyper::header::CONTENT_TYPE, "application/json");
        }
        builder
            .body(Full::new(Bytes::from(body.unwrap_or_default())))
            .expect("a valid request")
    }

    /// Send one request on a fresh connection and read the whole reply.
    pub async fn send(&self, request: hyper::Request<Full<Bytes>>) -> Result<Reply, ClientError> {
        let mut sender = self.connect().await?;
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| ClientError::Http(e.to_string()))?
            .to_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        Ok(Reply {
            status,
            body,
            text,
            headers,
        })
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> Result<Reply, ClientError> {
        self.send(Self::request("GET", path, token, None)).await
    }

    pub async fn post(
        &self,
        path: &str,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> Result<Reply, ClientError> {
        self.send(Self::request(
            "POST",
            path,
            token,
            Some(serde_json::to_vec(&body).expect("json")),
        ))
        .await
    }

    /// Redeem `code`: the started request on success (202), the reply
    /// otherwise. No token yet: the desktop must allow it first.
    pub async fn pair(
        &self,
        code: &str,
        device_name: &str,
    ) -> Result<crate::protocol::PairStarted, Reply> {
        let reply = self
            .post(
                "/v1/pair",
                None,
                serde_json::json!({"code": code, "device_name": device_name}),
            )
            .await
            .expect("the pairing request reaches the server");
        if reply.status != 202 {
            return Err(reply);
        }
        serde_json::from_value(reply.body.clone()).map_err(|_| reply)
    }

    /// Poll a pairing request once.
    pub async fn collect(&self, pairing_id: &str) -> Reply {
        self.get(&format!("/v1/pair/{pairing_id}"), None)
            .await
            .expect("the poll reaches the server")
    }

    /// Collect the token of an allowed request.
    pub async fn token(&self, pairing_id: &str) -> Result<String, Reply> {
        let reply = self.collect(pairing_id).await;
        match reply.body["token"].as_str() {
            Some(token) if reply.status == 200 => Ok(token.to_string()),
            _ => Err(reply),
        }
    }

    /// Open a session's event stream. The reply when it is not a 200.
    pub async fn events(
        &self,
        session_id: SessionId,
        token: Option<&str>,
        last_event_id: Option<&str>,
    ) -> Result<EventStream, Reply> {
        let mut request = Self::request(
            "GET",
            &format!("/v1/sessions/{session_id}/events"),
            token,
            None,
        );
        if let Some(id) = last_event_id {
            request.headers_mut().insert(
                "last-event-id",
                hyper::header::HeaderValue::from_str(id).expect("a header value"),
            );
        }
        let mut sender = self.connect().await.map_err(|e| Reply {
            status: 0,
            body: serde_json::Value::Null,
            text: e.to_string(),
            headers: hyper::HeaderMap::new(),
        })?;
        let response = sender.send_request(request).await.map_err(|e| Reply {
            status: 0,
            body: serde_json::Value::Null,
            text: e.to_string(),
            headers: hyper::HeaderMap::new(),
        })?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if status != 200 {
            let bytes = response
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            return Err(Reply {
                status,
                body: serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
                text: String::from_utf8_lossy(&bytes).into_owned(),
                headers,
            });
        }
        Ok(EventStream {
            body: response.into_body(),
            buffer: String::new(),
            ready: VecDeque::new(),
            comments: 0,
            ended: false,
            headers,
            _sender: sender,
        })
    }
}

/// One Server-Sent Event.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub id: String,
    pub event: String,
    pub data: serde_json::Value,
}

impl SseEvent {
    /// The sequence number in `id` (`<epoch>.<seq>`).
    pub fn seq(&self) -> u64 {
        self.id
            .rsplit_once('.')
            .and_then(|(_, seq)| seq.parse().ok())
            .expect("an id of the form <epoch>.<seq>")
    }

    pub fn epoch(&self) -> &str {
        self.id.rsplit_once('.').map(|(e, _)| e).unwrap_or("")
    }
}

pub struct EventStream {
    body: hyper::body::Incoming,
    buffer: String,
    ready: VecDeque<SseEvent>,
    comments: usize,
    ended: bool,
    pub headers: hyper::HeaderMap,
    _sender: Sender,
}

impl EventStream {
    /// The next event, waiting up to five seconds.
    pub async fn next(&mut self) -> Option<SseEvent> {
        self.next_within(Duration::from_secs(5)).await
    }

    /// The next event, or `None` when the stream ends or `limit` passes.
    pub async fn next_within(&mut self, limit: Duration) -> Option<SseEvent> {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if let Some(event) = self.ready.pop_front() {
                return Some(event);
            }
            if self.ended {
                return None;
            }
            match tokio::time::timeout_at(deadline, self.body.frame()).await {
                Err(_) => return None,
                Ok(None) | Ok(Some(Err(_))) => self.ended = true,
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        self.buffer.push_str(&String::from_utf8_lossy(&data));
                        self.parse();
                    }
                }
            }
        }
    }

    /// Whether the server ends the stream within `limit`. Events that
    /// arrive first are discarded.
    pub async fn ends_within(&mut self, limit: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            self.ready.clear();
            if self.ended {
                return true;
            }
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let _ = self.next_within(left).await;
        }
    }

    /// Keep-alive comments received so far.
    pub fn comments(&self) -> usize {
        self.comments
    }

    fn parse(&mut self) {
        while let Some(end) = self.buffer.find("\n\n") {
            let block: String = self.buffer.drain(..end + 2).collect();
            let (mut id, mut event, mut data) = (String::new(), String::new(), String::new());
            let mut comment = false;
            for line in block.lines() {
                if let Some(rest) = line.strip_prefix(':') {
                    let _ = rest;
                    comment = true;
                } else if let Some(rest) = line.strip_prefix("id: ") {
                    id = rest.into();
                } else if let Some(rest) = line.strip_prefix("event: ") {
                    event = rest.into();
                } else if let Some(rest) = line.strip_prefix("data: ") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(rest);
                }
            }
            if comment && data.is_empty() {
                self.comments += 1;
                continue;
            }
            self.ready.push_back(SseEvent {
                id,
                event,
                data: serde_json::from_str(&data).unwrap_or(serde_json::Value::Null),
            });
        }
    }
}

/// Whether nothing accepts TCP connections at `addr`.
pub async fn refuses_connections(addr: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr)).await,
        Ok(Err(_))
    )
}

/// Send a plaintext HTTP request to `addr` and return whatever comes back
/// before the connection closes (at most two seconds).
pub async fn plain_http(addr: SocketAddr) -> std::io::Result<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(addr).await?;
    tcp.write_all(b"GET /v1/hello HTTP/1.1\r\nHost: forge-remote\r\n\r\n")
        .await?;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(2), tcp.read_to_end(&mut out)).await;
    Ok(out)
}
