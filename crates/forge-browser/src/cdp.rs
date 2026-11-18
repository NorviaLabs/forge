//! A minimal Chrome DevTools Protocol client over a pipe.
//!
//! The browser is started with `--remote-debugging-pipe`: it reads commands
//! on fd 3 and writes replies and events on fd 4, each message one JSON
//! object followed by a NUL byte. No TCP debugging port is opened, so no
//! other local process can attach to the agent's browser.
//!
//! One reader task routes replies to the call that is waiting for them (by
//! `id`) and fans events out to subscribers. A call that outlives its
//! timeout, or a pipe that closes, fails rather than hangs.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, oneshot};

/// How long a call waits for its reply by default.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// An event the browser sent (`Page.loadEventFired`, …).
#[derive(Debug, Clone, PartialEq)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    /// The target session the event belongs to, for flattened sessions.
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CdpError {
    #[error("browser error {code}: {message}")]
    Protocol { code: i64, message: String },
    #[error("the browser closed the connection")]
    Closed,
    #[error("the browser did not answer `{0}` in time")]
    Timeout(String),
    #[error("browser pipe error: {0}")]
    Io(String),
}

/// A connection to one browser process.
#[derive(Clone)]
pub struct CdpConnection {
    inner: Arc<Inner>,
    timeout: Duration,
}

type Reply = Result<Value, CdpError>;
type Writer = Box<dyn AsyncWrite + Unpin + Send>;

struct Inner {
    writer: tokio::sync::Mutex<Writer>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    next_id: AtomicU64,
    events: broadcast::Sender<CdpEvent>,
    closed: AtomicBool,
}

impl Inner {
    fn pending(&self) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Reply>>> {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Mark the pipe closed and fail every waiting call.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        for (_, waiter) in self.pending().drain() {
            let _ = waiter.send(Err(CdpError::Closed));
        }
    }

    fn dispatch(&self, message: &[u8]) {
        let Ok(value) = serde_json::from_slice::<Value>(message) else {
            tracing::debug!("ignoring a browser message that is not JSON");
            return;
        };
        if let Some(id) = value.get("id").and_then(Value::as_u64) {
            let Some(waiter) = self.pending().remove(&id) else {
                return; // a reply nobody waits for any more (timed out)
            };
            let reply = match value.get("error") {
                Some(error) => Err(CdpError::Protocol {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error")
                        .to_string(),
                }),
                None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = waiter.send(reply);
        } else if let Some(method) = value.get("method").and_then(Value::as_str) {
            // No subscriber is fine: nobody is waiting for events.
            let _ = self.events.send(CdpEvent {
                method: method.to_string(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
                session_id: value
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
}

impl CdpConnection {
    /// Start routing messages read from `reader`; commands are written to
    /// `writer`. Must be called inside a Tokio runtime.
    pub fn spawn<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (events, _) = broadcast::channel(1024);
        let inner = Arc::new(Inner {
            writer: tokio::sync::Mutex::new(Box::new(writer)),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events,
            closed: AtomicBool::new(false),
        });
        let reading = Arc::downgrade(&inner);
        tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.read_until(0, &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if buf.last() == Some(&0) {
                    buf.pop();
                }
                let Some(inner) = reading.upgrade() else {
                    return;
                };
                inner.dispatch(&buf);
            }
            if let Some(inner) = reading.upgrade() {
                inner.close();
            }
        });
        Self {
            inner,
            timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// Use `timeout` for every call made through this handle.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Call a browser-level method.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.send(None, method, params).await
    }

    /// Call a method on an attached target session.
    pub async fn call_session(
        &self,
        session_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        self.send(Some(session_id), method, params).await
    }

    async fn send(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        if self.is_closed() {
            return Err(CdpError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending().insert(id, tx);
        // Closed between the check and the insert: nothing will answer.
        if self.is_closed() {
            self.inner.pending().remove(&id);
            return Err(CdpError::Closed);
        }
        let mut message = serde_json::json!({"id": id, "method": method, "params": params});
        if let Some(session_id) = session_id {
            message["sessionId"] = Value::String(session_id.to_string());
        }
        let mut bytes =
            serde_json::to_vec(&message).map_err(|error| CdpError::Io(error.to_string()))?;
        bytes.push(0);
        let written = {
            let mut writer = self.inner.writer.lock().await;
            match writer.write_all(&bytes).await {
                Ok(()) => writer.flush().await,
                Err(error) => Err(error),
            }
        };
        if written.is_err() {
            self.inner.pending().remove(&id);
            self.inner.close();
            return Err(CdpError::Closed);
        }
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                self.inner.pending().remove(&id);
                Err(CdpError::Timeout(method.to_string()))
            }
        }
    }

    /// Receive every event sent from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    /// Whether the pipe has closed.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A fake browser on the far end of two in-memory pipes: it reads
    /// NUL-terminated commands and answers each through `reply`.
    fn fake_browser<F>(reply: F) -> CdpConnection
    where
        F: Fn(Value) -> Vec<Value> + Send + 'static,
    {
        let (ours_w, theirs_r) = tokio::io::duplex(1 << 16);
        let (theirs_w, ours_r) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let mut reader = BufReader::new(theirs_r);
            let mut writer = theirs_w;
            loop {
                let mut buf = Vec::new();
                if reader.read_until(0, &mut buf).await.unwrap_or(0) == 0 {
                    break;
                }
                buf.pop();
                let command: Value = serde_json::from_slice(&buf).unwrap();
                for message in reply(command) {
                    let mut bytes = serde_json::to_vec(&message).unwrap();
                    bytes.push(0);
                    if writer.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
            }
        });
        CdpConnection::spawn(ours_r, ours_w)
    }

    #[tokio::test]
    async fn a_call_returns_its_result() {
        let cdp = fake_browser(|command| {
            assert_eq!(command["method"], "Browser.getVersion");
            vec![json!({"id": command["id"], "result": {"product": "Chrome/1"}})]
        });
        let result = cdp.call("Browser.getVersion", json!({})).await.unwrap();
        assert_eq!(result["product"], "Chrome/1");
    }

    #[tokio::test]
    async fn a_session_call_carries_its_session_id() {
        let cdp = fake_browser(|command| {
            assert_eq!(command["sessionId"], "S1");
            assert_eq!(command["params"]["expression"], "1+1");
            vec![json!({"id": command["id"], "sessionId": "S1", "result": {"value": 2}})]
        });
        let result = cdp
            .call_session("S1", "Runtime.evaluate", json!({"expression": "1+1"}))
            .await
            .unwrap();
        assert_eq!(result["value"], 2);
    }

    #[tokio::test]
    async fn a_protocol_error_is_returned_as_such() {
        let cdp = fake_browser(|command| {
            vec![json!({"id": command["id"], "error": {"code": -32000, "message": "No node"}})]
        });
        let error = cdp.call("DOM.focus", json!({})).await.unwrap_err();
        assert_eq!(
            error,
            CdpError::Protocol {
                code: -32000,
                message: "No node".into()
            }
        );
    }

    /// Replies are matched by id, so an event or another call's reply in
    /// between does not confuse the caller.
    #[tokio::test]
    async fn replies_are_routed_by_id_and_events_are_fanned_out() {
        let cdp = fake_browser(|command| {
            let id = command["id"].as_u64().unwrap();
            vec![
                json!({"method": "Page.loadEventFired", "params": {"timestamp": 1}, "sessionId": "S"}),
                json!({"id": id + 1000, "result": {"stray": true}}),
                json!({"id": id, "result": {"mine": id}}),
            ]
        });
        let mut events = cdp.subscribe();
        let (a, b) = tokio::join!(cdp.call("A.one", json!({})), cdp.call("B.two", json!({})));
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_ne!(a["mine"], b["mine"]);
        let event = events.recv().await.unwrap();
        assert_eq!(event.method, "Page.loadEventFired");
        assert_eq!(event.session_id.as_deref(), Some("S"));
    }

    #[tokio::test]
    async fn a_silent_browser_times_out() {
        let cdp = fake_browser(|_| vec![]).with_timeout(Duration::from_millis(100));
        let error = cdp.call("Page.navigate", json!({})).await.unwrap_err();
        assert_eq!(error, CdpError::Timeout("Page.navigate".into()));
    }

    #[tokio::test]
    async fn a_closed_pipe_fails_waiting_and_later_calls() {
        let (ours_w, theirs_r) = tokio::io::duplex(1024);
        let (theirs_w, ours_r) = tokio::io::duplex(1024);
        let cdp = CdpConnection::spawn(ours_r, ours_w);
        let pending = tokio::spawn({
            let cdp = cdp.clone();
            async move { cdp.call("Page.navigate", json!({})).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(theirs_w);
        drop(theirs_r);
        assert_eq!(pending.await.unwrap().unwrap_err(), CdpError::Closed);
        assert!(cdp.is_closed());
        assert_eq!(
            cdp.call("Page.reload", json!({})).await.unwrap_err(),
            CdpError::Closed
        );
    }
}
