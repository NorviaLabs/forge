//! The HTTPS API (protocol v1): routing, authentication, limits, and the
//! Server-Sent Events writer. Driven by [`crate::service::RemoteService`];
//! its behaviour is tested end to end in `tests/`.
//!
//! | Route | |
//! |---|---|
//! | `POST /v1/pair` | a code for a token (no auth) |
//! | `GET /v1/hello` | token check |
//! | `GET /v1/sessions` | active sessions of open projects |
//! | `GET /v1/sessions/{id}/events` | SSE, resumable with `Last-Event-ID` |
//! | `POST /v1/sessions/{id}/prompt` | queue a prompt |
//! | `POST /v1/sessions/{id}/approval` | allow once, deny, deny with note |
//! | `POST /v1/sessions/{id}/stop` | stop the running turn |
//!
//! Every refusal for a missing, malformed, wrong or revoked token is the
//! same `401`, so a caller learns nothing about which it was.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use forge_types::SessionId;
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited, StreamBody};
use hyper::body::{Frame as BodyFrame, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::activity::Activity;
use crate::backend::BackendError;
use crate::hub::{EventId, Frame, Hub, Subscription};
use crate::pairing::Poll;
use crate::protocol::{
    ApprovalRequest, Hello, PairPending, PairRequest, PairResponse, PairStarted, PromptRequest,
    RemoteDecision, SessionList, PROTOCOL_VERSION,
};
use crate::service::{Inner, StreamGuard};
use crate::store::{clean_device_name, Device};

type Body = BoxBody<Bytes, Infallible>;

/// What every connection of one listener shares.
pub(crate) struct Context {
    pub(crate) service: Arc<Inner>,
    pub(crate) hub: Arc<Hub>,
    /// Cancelled when the listener stops: every connection ends.
    pub(crate) cancel: CancellationToken,
}

/// Accept connections until cancelled, then drop the listener and abort
/// every connection before returning.
pub(crate) async fn serve(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    context: Arc<Context>,
) {
    let places = Arc::new(Semaphore::new(context.service.limits.max_connections));
    let peers = Arc::new(PeerCounts::default());
    let per_peer = context.service.limits.max_connections_per_peer;
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = context.cancel.cancelled() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let (tcp, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        // Out of descriptors and the like: back off rather
                        // than spin.
                        tracing::debug!(%error, "remote: accept failed");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                // Over either cap: hang up before the handshake costs
                // anything. The per-address cap keeps one host on the
                // network from taking every place.
                let Some(peer_place) = peers.clone().enter(peer.ip(), per_peer) else {
                    drop(tcp);
                    continue;
                };
                let Ok(place) = places.clone().try_acquire_owned() else {
                    drop(tcp);
                    continue;
                };
                let (acceptor, context) = (acceptor.clone(), context.clone());
                connections.spawn(async move {
                    let _places = (place, peer_place);
                    connection(tcp, acceptor, context).await;
                });
            }
        }
    }
    drop(listener);
    connections.shutdown().await;
}

/// Open connections per remote address.
#[derive(Default)]
struct PeerCounts(std::sync::Mutex<std::collections::HashMap<std::net::IpAddr, usize>>);

impl PeerCounts {
    /// Take a place for `ip`, unless it already holds `max`.
    fn enter(self: Arc<Self>, ip: std::net::IpAddr, max: usize) -> Option<PeerPlace> {
        let mut counts = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let count = counts.entry(ip).or_insert(0);
        if *count >= max {
            return None;
        }
        *count += 1;
        drop(counts);
        Some(PeerPlace { counts: self, ip })
    }
}

/// One connection's place for its address; freed on drop.
struct PeerPlace {
    counts: Arc<PeerCounts>,
    ip: std::net::IpAddr,
}

impl Drop for PeerPlace {
    fn drop(&mut self) {
        let mut counts = self.counts.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = counts.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.ip);
            }
        }
    }
}

async fn connection(
    tcp: tokio::net::TcpStream,
    acceptor: tokio_rustls::TlsAcceptor,
    context: Arc<Context>,
) {
    let limits = &context.service.limits;
    let tls = match tokio::time::timeout(limits.header_timeout, acceptor.accept(tcp)).await {
        Ok(Ok(tls)) => tls,
        // Not TLS, the wrong certificate for the client, or too slow.
        _ => return,
    };
    let service = {
        let context = context.clone();
        hyper::service::service_fn(move |request| {
            let context = context.clone();
            async move { Ok::<_, Infallible>(handle(context, request).await) }
        })
    };
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(limits.header_timeout);
    let served = builder.serve_connection(TokioIo::new(tls), service);
    tokio::select! {
        _ = served => {}
        () = context.cancel.cancelled() => {}
    }
}

async fn handle(context: Arc<Context>, request: Request<Incoming>) -> Response<Body> {
    let path = request.uri().path().to_string();
    if path == "/v1/pair" {
        return if request.method() == Method::POST {
            pair(&context, request).await
        } else {
            method_not_allowed()
        };
    }
    // Collecting a pairing needs no token: the pairing id is the secret.
    if let Some(pairing_id) = path.strip_prefix("/v1/pair/") {
        return if request.method() == Method::GET {
            collect(&context, pairing_id)
        } else {
            method_not_allowed()
        };
    }
    // Everything else needs the paired device's token, before the route is
    // even looked at: an unauthenticated caller learns nothing.
    let Some(device) = authenticate(&context, &request) else {
        context.service.note_auth_failure();
        return unauthorized();
    };
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let method = request.method().clone();
    match segments.as_slice() {
        ["v1", "hello"] => match method {
            Method::GET => json_response(
                StatusCode::OK,
                &Hello {
                    protocol: PROTOCOL_VERSION,
                    device_name: device.name,
                },
            ),
            _ => method_not_allowed(),
        },
        ["v1", "sessions"] => match method {
            Method::GET => match context.service.backend().sessions().await {
                Ok(sessions) => json_response(StatusCode::OK, &SessionList { sessions }),
                Err(error) => backend_error(error),
            },
            _ => method_not_allowed(),
        },
        ["v1", "sessions", id, action] => {
            let Ok(session_id) = id.parse::<SessionId>() else {
                return error(StatusCode::NOT_FOUND, "that session does not exist");
            };
            if method == Method::POST && matches!(*action, "prompt" | "approval" | "stop") {
                if let Err(wait) = context.service.take_action() {
                    return too_many("too many actions this minute; wait and try again", wait);
                }
            }
            match (*action, method) {
                ("events", Method::GET) => events(&context, &request, session_id).await,
                ("messages", Method::GET) => messages(&context, &request, session_id).await,
                ("prompt", Method::POST) => prompt(&context, request, &device, session_id).await,
                ("approval", Method::POST) => {
                    approval(&context, request, &device, session_id).await
                }
                ("stop", Method::POST) => stop(&context, &device, session_id).await,
                ("events" | "messages" | "prompt" | "approval" | "stop", _) => method_not_allowed(),
                _ => not_found(),
            }
        }
        _ => not_found(),
    }
}

/// The paired device, when the request carries its token as
/// `Authorization: Bearer <token>`. Anything else is `None`.
fn authenticate(context: &Context, request: &Request<Incoming>) -> Option<Device> {
    let value = request
        .headers()
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    context.service.store.authenticate(token)
}

async fn pair(context: &Context, request: Request<Incoming>) -> Response<Body> {
    let body: PairRequest = match read_json(context, request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    // The name is checked before the code, so a bad request never uses a
    // guess up.
    let Some(name) = clean_device_name(&body.device_name) else {
        return error(
            StatusCode::BAD_REQUEST,
            "device_name must be 1 to 64 characters with no control characters",
        );
    };
    let service = &context.service;
    if let Err(refused) = service.pairing.redeem(&body.code) {
        // Logged, and a burnt code is explained on the desktop.
        service.note_pairing_failure(refused == crate::pairing::PairingError::Exhausted);
        return error(StatusCode::UNAUTHORIZED, "invalid or expired code");
    }
    // The code opens a request; no token until the desktop allows it.
    let opened = service.requests.open(&name);
    let _ = service
        .activity
        .record(Activity::PairingRequested { device: name });
    service.publish();
    service.publish_later(crate::pairing::REQUEST_TTL);
    json_response(
        StatusCode::ACCEPTED,
        &PairStarted {
            pairing_id: opened.pairing_id,
            confirmation: opened.confirmation,
            expires_at: opened.expires_at,
        },
    )
}

/// `GET /v1/pair/{pairing_id}`: pending, the token (once), refused, or
/// nothing.
fn collect(context: &Context, pairing_id: &str) -> Response<Body> {
    let service = &context.service;
    let poll = service.requests.poll(pairing_id);
    // A poll can find the request expired: the desktop hears at once.
    service.publish();
    match poll {
        Poll::Pending => {
            let mut response = json_response(
                StatusCode::ACCEPTED,
                &PairPending {
                    status: "pending".into(),
                },
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            response
        }
        Poll::Refused => error(StatusCode::FORBIDDEN, "pairing was refused on the desktop"),
        Poll::Unknown => error(StatusCode::NOT_FOUND, "no such pairing request"),
        Poll::Allowed { device_name } => {
            // The device record and its token exist from this moment; the
            // device they replace stops working now.
            let (device, token) = match service.store.pair(&device_name) {
                Ok(paired) => paired,
                Err(why) => {
                    tracing::warn!(%why, "remote: pairing could not be saved");
                    return error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "pairing could not be saved",
                    );
                }
            };
            service.end_streams();
            let _ = service.activity.record(Activity::Paired {
                device: device.name.clone(),
            });
            service.publish();
            json_response(
                StatusCode::OK,
                &PairResponse {
                    token: token.as_str().to_string(),
                    device_id: device.id,
                    device_name: device.name,
                },
            )
        }
    }
}

async fn events(
    context: &Context,
    request: &Request<Incoming>,
    session_id: SessionId,
) -> Response<Body> {
    // A present but unreadable id still means "I have seen events": it
    // gets a snapshot marked resync, never a silent fresh start.
    let resume = request.headers().get("last-event-id").map(|value| {
        value
            .to_str()
            .ok()
            .and_then(EventId::parse)
            .unwrap_or(EventId {
                epoch: String::new(),
                seq: 0,
            })
    });
    let Some(guard) = context.service.open_stream() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many event streams are open; close one first",
        );
    };
    let subscription = match context.hub.follow(session_id, resume).await {
        Ok(subscription) => subscription,
        Err(error) => return backend_error(error),
    };
    let mut keep_alive = tokio::time::interval_at(
        tokio::time::Instant::now() + context.service.limits.keep_alive,
        context.service.limits.keep_alive,
    );
    keep_alive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let state = SseState {
        subscription,
        keep_alive,
        generation: context.service.generation(),
        cancel: context.cancel.clone(),
        _guard: guard,
    };
    let stream = futures::stream::unfold(state, |mut state| async move {
        let bytes = tokio::select! {
            biased;
            () = state.cancel.cancelled() => return None,
            // The device was revoked or replaced.
            _ = state.generation.changed() => return None,
            frame = state.subscription.recv() => sse(&frame?),
            _ = state.keep_alive.tick() => Bytes::from_static(b": keep-alive\n\n"),
        };
        Some((Ok::<_, Infallible>(BodyFrame::data(bytes)), state))
    });
    let mut response = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

struct SseState {
    subscription: Subscription,
    keep_alive: tokio::time::Interval,
    generation: tokio::sync::watch::Receiver<u64>,
    cancel: CancellationToken,
    _guard: StreamGuard,
}

/// One SSE event. `data` is single-line JSON: newlines inside strings are
/// escaped.
fn sse(frame: &Frame) -> Bytes {
    Bytes::from(format!(
        "id: {}\nevent: {}\ndata: {}\n\n",
        frame.id, frame.event, frame.data
    ))
}

/// `?before=<index>&limit=<n>`: older messages for scroll-back.
async fn messages(
    context: &Context,
    request: &Request<Incoming>,
    session_id: SessionId,
) -> Response<Body> {
    let mut before = None;
    let mut limit = crate::protocol::DEFAULT_PAGE;
    for pair in request.uri().query().unwrap_or("").split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let Ok(number) = value.parse::<usize>() else {
            return error(
                StatusCode::BAD_REQUEST,
                "before and limit are whole numbers",
            );
        };
        match key {
            "before" => before = Some(number),
            "limit" => limit = number,
            _ => {}
        }
    }
    match context
        .service
        .backend()
        .messages(session_id, before, limit)
        .await
    {
        Ok(page) => json_response(StatusCode::OK, &page),
        Err(failure) => backend_error(failure),
    }
}

async fn prompt(
    context: &Context,
    request: Request<Incoming>,
    device: &Device,
    session_id: SessionId,
) -> Response<Body> {
    let body: PromptRequest = match read_json(context, request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    if body.text.trim().is_empty() {
        return error(StatusCode::BAD_REQUEST, "the prompt is empty");
    }
    let limit = context.service.limits.max_prompt;
    if body.text.len() > limit {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("a prompt can be at most {limit} bytes"),
        );
    }
    // A device may not pile prompts onto a session faster than it works.
    match context.service.backend().session_state(session_id).await {
        Ok(state) if state.queue.len() >= context.service.limits.max_remote_queue => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "this session already has prompts waiting; try again when it has worked through them",
            );
        }
        Ok(_) => {}
        Err(failure) => return backend_error(failure),
    }
    let chars = body.text.chars().count();
    match context
        .service
        .backend()
        .submit_prompt(session_id, body.text)
        .await
    {
        Ok(()) => {
            let _ = context.service.activity.record(Activity::Prompt {
                device: device.name.clone(),
                session_id,
                chars,
            });
            accepted()
        }
        Err(error) => backend_error(error),
    }
}

async fn approval(
    context: &Context,
    request: Request<Incoming>,
    device: &Device,
    session_id: SessionId,
) -> Response<Body> {
    let body: ApprovalRequest = match read_json(context, request).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let note = match body.decision {
        RemoteDecision::DenyWithNote => {
            let note = body.note.as_deref().unwrap_or("").trim().to_string();
            if note.is_empty() {
                return error(
                    StatusCode::BAD_REQUEST,
                    "a note is required for this choice",
                );
            }
            let limit = context.service.limits.max_note;
            if note.len() > limit {
                return error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("a note can be at most {limit} bytes"),
                );
            }
            Some(note)
        }
        RemoteDecision::AllowOnce | RemoteDecision::Deny => None,
    };
    let actor = format!("remote:{}", device.name);
    match context
        .service
        .backend()
        .decide(session_id, &body.call_id, body.decision, note, &actor)
        .await
    {
        Ok(()) => {
            let _ = context.service.activity.record(Activity::Decision {
                device: device.name.clone(),
                session_id,
                call_id: body.call_id,
                decision: body.decision.as_str().to_string(),
            });
            accepted()
        }
        Err(error) => backend_error(error),
    }
}

async fn stop(context: &Context, device: &Device, session_id: SessionId) -> Response<Body> {
    match context.service.backend().stop(session_id).await {
        Ok(()) => {
            let _ = context.service.activity.record(Activity::Stop {
                device: device.name.clone(),
                session_id,
            });
            accepted()
        }
        Err(error) => backend_error(error),
    }
}

/// The body as `T`, within the size limit.
async fn read_json<T: DeserializeOwned>(
    context: &Context,
    request: Request<Incoming>,
) -> Result<T, Response<Body>> {
    let limit = context.service.limits.max_body;
    let too_large = || error(StatusCode::PAYLOAD_TOO_LARGE, "the request is too large");
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared.is_some_and(|length| length > limit) {
        return Err(too_large());
    }
    // Headers are timed by hyper; the body gets its own deadline, so a
    // client trickling bytes cannot hold a connection place open.
    let deadline = context.service.limits.body_timeout;
    let bytes =
        match tokio::time::timeout(deadline, Limited::new(request.into_body(), limit).collect())
            .await
        {
            Ok(Ok(collected)) => collected.to_bytes(),
            Ok(Err(_)) => return Err(too_large()),
            Err(_) => {
                let mut response = error(
                    StatusCode::REQUEST_TIMEOUT,
                    "the request body took too long",
                );
                response
                    .headers_mut()
                    .insert(header::CONNECTION, HeaderValue::from_static("close"));
                return Err(response);
            }
        };
    serde_json::from_slice(&bytes).map_err(|why| {
        error(
            StatusCode::BAD_REQUEST,
            &format!("the request body is not what this route takes: {why}"),
        )
    })
}

fn backend_error(failure: BackendError) -> Response<Body> {
    let status = match &failure {
        BackendError::UnknownSession => StatusCode::NOT_FOUND,
        BackendError::NotPending => StatusCode::CONFLICT,
        BackendError::NotAllowed(_) => StatusCode::FORBIDDEN,
        BackendError::Invalid(_) => StatusCode::BAD_REQUEST,
        BackendError::Unavailable(detail) => {
            // Internal failures can name local paths: the detail goes to
            // the log, the device gets a sentence.
            tracing::warn!(%detail, "remote: a request failed inside Forge");
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Forge could not do that right now; try again",
            );
        }
    };
    error(status, &failure.to_string())
}

fn json_response<T: serde::Serialize>(status: StatusCode, body: &T) -> Response<Body> {
    let bytes = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::new(full(bytes));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // Responses may carry a token or a transcript: never cached.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn full(bytes: Vec<u8>) -> Body {
    BodyExt::boxed(Full::new(Bytes::from(bytes)))
}

fn error(status: StatusCode, message: &str) -> Response<Body> {
    json_response(status, &json!({ "error": message }))
}

fn too_many(message: &str, wait: Duration) -> Response<Body> {
    let mut response = error(StatusCode::TOO_MANY_REQUESTS, message);
    let seconds = wait.as_secs().max(1).to_string();
    if let Ok(value) = HeaderValue::from_str(&seconds) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn unauthorized() -> Response<Body> {
    let mut response = error(StatusCode::UNAUTHORIZED, "unauthorized");
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn not_found() -> Response<Body> {
    error(StatusCode::NOT_FOUND, "no such route")
}

fn method_not_allowed() -> Response<Body> {
    error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
}

fn accepted() -> Response<Body> {
    json_response(StatusCode::ACCEPTED, &json!({ "accepted": true }))
}
