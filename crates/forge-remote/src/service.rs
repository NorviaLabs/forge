//! The service: settings, the listener's lifecycle, pairing, revoking, and
//! the status the desktop shows.
//!
//! Nothing listens until the settings say `enabled`. Turning it off cancels
//! the accept loop, drops the listener, ends every connection and withdraws
//! any pairing code.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::activity::{Activity, ActivityLog};
use crate::backend::RemoteBackend;
use crate::clock::Clock;
use crate::http;
use crate::hub::{Hub, HubConfig};
use crate::pairing::{Pairing, PairingRequests, CODE_TTL};
use crate::settings::{detect_lan_address, Reach, RemoteSettings, SettingsError};
use crate::store::{Device, DeviceStore, StoreError};
use crate::tls::{TlsError, TlsIdentity};

/// Limits on what a request may cost.
#[derive(Debug, Clone)]
pub struct Limits {
    /// The largest request body.
    pub max_body: usize,
    /// The longest prompt, in bytes.
    pub max_prompt: usize,
    /// The longest deny note, in bytes.
    pub max_note: usize,
    /// How long a connection may take to send its request headers.
    pub header_timeout: Duration,
    /// How long a request may take to send its body, once its headers are in.
    pub body_timeout: Duration,
    /// Open connections at once.
    pub max_connections: usize,
    /// Open connections from one address at once, so no single host on the
    /// network can take every place. Room for a device's four event streams
    /// and its requests.
    pub max_connections_per_peer: usize,
    /// Open event streams at once.
    pub max_streams: usize,
    /// How often an idle event stream gets a keep-alive comment.
    pub keep_alive: Duration,
    /// Prompts, decisions and stops a device may send per minute.
    pub max_actions_per_minute: usize,
    /// A remote prompt is refused while the session already has this many
    /// prompts waiting.
    pub max_remote_queue: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body: 64 * 1024,
            max_prompt: 32 * 1024,
            max_note: 4 * 1024,
            header_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(10),
            max_connections: 16,
            max_connections_per_peer: 8,
            max_streams: 4,
            keep_alive: Duration::from_secs(15),
            max_actions_per_minute: 30,
            max_remote_queue: 10,
        }
    }
}

pub struct ServiceOptions {
    /// The remote folder (`<Forge data dir>/remote` in the app).
    pub dir: PathBuf,
    pub backend: Arc<dyn RemoteBackend>,
    pub clock: Arc<dyn Clock>,
    pub limits: Limits,
    pub hub: HubConfig,
}

/// What the desktop shows about remote control.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemoteStatus {
    pub enabled: bool,
    pub reach: Reach,
    /// The configured address and port, for the Settings fields.
    pub address: Option<String>,
    pub port: u16,
    /// Where it is listening, while it is.
    pub listening: Option<SocketAddr>,
    /// Why it is not listening although enabled (port in use, bad address,
    /// damaged settings).
    pub error: Option<String>,
    /// The certificate fingerprint, in groups, once there is one.
    pub fingerprint: Option<String>,
    pub device: Option<Device>,
    /// An event stream of the paired device is open.
    pub connected: bool,
    /// When the pairing code on screen expires, while there is one.
    pub pairing_expires_at: Option<DateTime<Utc>>,
    /// Why the last pairing code stopped working early (wrong guesses used
    /// it up), until a new one is made.
    pub pairing_error: Option<String>,
    /// A device that redeemed the code and waits for Allow or Refuse.
    pub pairing_request: Option<crate::pairing::RequestView>,
    /// A LAN address suggestion for the Settings field.
    pub suggested_address: Option<String>,
}

/// What the desktop shows while a device pairs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingInfo {
    /// `ABCD-EFGH`.
    pub code: String,
    pub expires_at: DateTime<Utc>,
    /// `forge-remote://host:port/?code=ABCD-EFGH&fp=sha256:<hex>`.
    pub link: String,
    /// The certificate fingerprint, in groups.
    pub fingerprint: String,
    /// The device this pairing will replace, if one is paired.
    pub replaces: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Tls(#[from] TlsError),
    #[error("remote control is not listening")]
    NotListening,
    #[error(transparent)]
    Pairing(#[from] crate::pairing::DecideError),
}

#[derive(Clone)]
pub struct RemoteService {
    inner: Arc<Inner>,
}

/// A listener that is up.
struct Running {
    addr: SocketAddr,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    hub: Arc<Hub>,
}

/// Refused requests since the last summary line.
#[derive(Default)]
struct AuthFailures {
    pending: u64,
    written: Option<Instant>,
}

/// How often refused requests are summarised in the activity log.
const AUTH_FAILURE_SUMMARY: Duration = Duration::from_secs(60);

/// Shared by the service handle and every connection.
pub(crate) struct Inner {
    dir: PathBuf,
    backend: Arc<dyn RemoteBackend>,
    clock: Arc<dyn Clock>,
    pub(crate) limits: Limits,
    hub_config: HubConfig,
    settings: Mutex<RemoteSettings>,
    /// Why the saved settings could not be read (the service stays off).
    settings_error: Mutex<Option<String>>,
    /// Why the listener is not up although enabled.
    listen_error: Mutex<Option<String>>,
    pub(crate) store: DeviceStore,
    pub(crate) pairing: Pairing,
    /// The device that redeemed the code, waiting for Allow or Refuse.
    pub(crate) requests: PairingRequests,
    pub(crate) activity: ActivityLog,
    identity: Mutex<Option<Arc<TlsIdentity>>>,
    running: tokio::sync::Mutex<Option<Running>>,
    /// Where the listener is, readable without the async lock.
    listening: Mutex<Option<SocketAddr>>,
    status: watch::Sender<RemoteStatus>,
    /// Bumped when the paired device changes or is revoked: every open
    /// stream ends, so a token that no longer works holds nothing open.
    generation: watch::Sender<u64>,
    /// Open event streams.
    streams: AtomicUsize,
    auth_failures: Mutex<AuthFailures>,
    pairing_failures: Mutex<AuthFailures>,
    pairing_error: Mutex<Option<String>>,
    /// When the device's recent actions were taken, for the per-minute limit.
    actions: Mutex<VecDeque<Instant>>,
    suggested_address: Option<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl RemoteService {
    /// Load settings, the paired device and (lazily) the TLS identity from
    /// `options.dir`. Does not listen: see [`Self::start_if_enabled`].
    pub fn new(options: ServiceOptions) -> Self {
        let (settings, settings_error) = match RemoteSettings::load(&options.dir) {
            Ok(settings) => (settings, None),
            Err(error) => (RemoteSettings::default(), Some(error.to_string())),
        };
        let store = DeviceStore::open(&options.dir, options.clock.clone());
        let activity = ActivityLog::new(&options.dir, options.clock.clone());
        let pairing = Pairing::new(options.clock.clone());
        let requests = PairingRequests::new(options.clock.clone());
        let placeholder = RemoteStatus {
            enabled: false,
            reach: Reach::Loopback,
            address: None,
            port: settings.port,
            listening: None,
            error: None,
            fingerprint: None,
            device: None,
            connected: false,
            pairing_expires_at: None,
            pairing_error: None,
            pairing_request: None,
            suggested_address: None,
        };
        let inner = Arc::new(Inner {
            dir: options.dir,
            backend: options.backend,
            clock: options.clock,
            limits: options.limits,
            hub_config: options.hub,
            settings: Mutex::new(settings),
            settings_error: Mutex::new(settings_error),
            listen_error: Mutex::new(None),
            store,
            pairing,
            requests,
            activity,
            identity: Mutex::new(None),
            running: tokio::sync::Mutex::new(None),
            listening: Mutex::new(None),
            status: watch::channel(placeholder).0,
            generation: watch::channel(0).0,
            streams: AtomicUsize::new(0),
            auth_failures: Mutex::new(AuthFailures::default()),
            pairing_failures: Mutex::new(AuthFailures::default()),
            pairing_error: Mutex::new(None),
            actions: Mutex::new(VecDeque::new()),
            suggested_address: detect_lan_address().map(|ip| ip.to_string()),
        });
        inner.publish();
        Self { inner }
    }

    pub fn settings(&self) -> RemoteSettings {
        lock(&self.inner.settings).clone()
    }

    /// The status now: time-based parts (a code or request running out)
    /// are brought up to date first, notifying watchers only on a change.
    pub fn status(&self) -> RemoteStatus {
        self.inner.publish();
        self.inner.status.borrow().clone()
    }

    /// Every status change, including pairing and connections.
    pub fn subscribe_status(&self) -> watch::Receiver<RemoteStatus> {
        self.inner.status.subscribe()
    }

    /// Listen if the saved settings say so (at app start).
    pub async fn start_if_enabled(&self) -> RemoteStatus {
        let settings = self.settings();
        if settings.enabled {
            self.inner.apply(&settings).await;
        }
        self.inner.publish();
        self.status()
    }

    /// Save `settings` and apply them: start, stop or restart the listener.
    /// A listener that cannot start (port in use) is reported in the
    /// status, not as an error.
    pub async fn configure(&self, settings: RemoteSettings) -> Result<RemoteStatus, RemoteError> {
        if settings.enabled {
            settings.bind_addr()?;
        }
        settings.save(&self.inner.dir)?;
        let was_enabled =
            std::mem::replace(&mut *lock(&self.inner.settings), settings.clone()).enabled;
        *lock(&self.inner.settings_error) = None;
        self.inner.apply(&settings).await;
        if was_enabled && !settings.enabled {
            let _ = self.inner.activity.record(Activity::Disabled);
        }
        self.inner.publish();
        Ok(self.status())
    }

    /// Issue a pairing code. Only while listening.
    pub fn start_pairing(&self) -> Result<PairingInfo, RemoteError> {
        let addr = (*lock(&self.inner.listening)).ok_or(RemoteError::NotListening)?;
        let identity = lock(&self.inner.identity)
            .clone()
            .ok_or(RemoteError::NotListening)?;
        // A new code ends any request the last one opened.
        self.inner.requests.cancel();
        let ticket = self.inner.pairing.start();
        *lock(&self.inner.pairing_error) = None;
        let fingerprint = identity.fingerprint();
        let info = PairingInfo {
            link: format!(
                "forge-remote://{addr}/?code={}&fp={}",
                ticket.code,
                fingerprint.to_pin()
            ),
            code: ticket.code,
            expires_at: ticket.expires_at,
            fingerprint: fingerprint.display_groups(),
            replaces: self.inner.store.device().map(|device| device.name),
        };
        self.inner.publish();
        // Say when the code runs out, so the page need not poll.
        self.inner.publish_later(CODE_TTL);
        Ok(info)
    }

    /// Allow or refuse the device waiting to pair. `confirmation` is the
    /// number the desktop showed, so a replaced request is never decided.
    pub async fn decide_pairing(
        &self,
        confirmation: &str,
        allow: bool,
    ) -> Result<RemoteStatus, RemoteError> {
        let device = self.inner.requests.decide(confirmation, allow);
        self.inner.publish();
        let device = device?;
        // An allowed device pairs when it collects its token; that is
        // recorded then.
        if !allow {
            let _ = self
                .inner
                .activity
                .record(Activity::PairingDeclined { device });
        }
        Ok(self.status())
    }

    pub fn cancel_pairing(&self) {
        self.inner.pairing.cancel();
        self.inner.requests.cancel();
        *lock(&self.inner.pairing_error) = None;
        self.inner.publish();
    }

    /// Forget the paired device and end its connections now. The TLS
    /// identity is replaced too, so a copy of the old certificate key can
    /// never impersonate Forge to the next device.
    pub async fn revoke(&self) -> Result<(), RemoteError> {
        if let Some(device) = self.inner.store.revoke()? {
            let _ = self.inner.activity.record(Activity::Revoked {
                device: device.name,
            });
            self.inner.end_streams();
            self.inner.rotate_identity().await?;
        }
        self.inner.end_streams();
        self.inner.publish();
        Ok(())
    }

    /// Stop listening without changing the settings (the app is quitting).
    pub async fn shutdown(&self) {
        self.inner.stop().await;
        let _ = self.inner.store.flush();
        self.inner.flush_auth_failures();
        self.inner.publish();
    }
}

impl Inner {
    /// Bring the listener in line with `settings`. Holds the lifecycle
    /// lock throughout, so two changes never interleave.
    async fn apply(self: &Arc<Self>, settings: &RemoteSettings) {
        let mut running = self.running.lock().await;
        self.stop_locked(&mut running).await;
        *lock(&self.listen_error) = None;
        if !settings.enabled {
            return;
        }
        match self.listen(settings).await {
            Ok(started) => {
                let _ = self.activity.record(Activity::Enabled {
                    address: started.addr.to_string(),
                });
                *lock(&self.listening) = Some(started.addr);
                *running = Some(started);
            }
            Err(why) => *lock(&self.listen_error) = Some(why),
        }
    }

    async fn listen(self: &Arc<Self>, settings: &RemoteSettings) -> Result<Running, String> {
        let bind = settings.bind_addr().map_err(|e| e.to_string())?;
        // Read the slot in its own statement: a guard in a `match`
        // scrutinee would live through the arms and deadlock the write.
        let loaded = lock(&self.identity).clone();
        let identity = match loaded {
            Some(identity) => identity,
            None => {
                let identity =
                    Arc::new(TlsIdentity::load_or_create(&self.dir).map_err(|e| e.to_string())?);
                *lock(&self.identity) = Some(identity.clone());
                identity
            }
        };
        let tls = identity.server_config().map_err(|e| e.to_string())?;
        let listener =
            tokio::net::TcpListener::bind(bind)
                .await
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::AddrInUse => format!(
                        "Port {} is in use. Choose another port and turn it on again.",
                        bind.port()
                    ),
                    std::io::ErrorKind::AddrNotAvailable => format!(
                    "{} is not an address of this computer. Choose another and turn it on again.",
                    bind.ip()
                ),
                    _ => format!("Cannot listen on {bind}: {error}"),
                })?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let hub = Arc::new(Hub::start(self.backend.clone(), self.hub_config.clone()));
        let cancel = CancellationToken::new();
        let context = Arc::new(http::Context {
            service: self.clone(),
            hub: hub.clone(),
            cancel: cancel.clone(),
        });
        let task = tokio::spawn(http::serve(
            listener,
            tokio_rustls::TlsAcceptor::from(tls),
            context,
        ));
        Ok(Running {
            addr,
            cancel,
            task,
            hub,
        })
    }

    /// Replace the TLS identity; a running listener restarts on the new one.
    async fn rotate_identity(self: &Arc<Self>) -> Result<(), RemoteError> {
        let mut running = self.running.lock().await;
        let fresh = Arc::new(TlsIdentity::replace(&self.dir)?);
        *lock(&self.identity) = Some(fresh);
        if running.is_some() {
            let settings = lock(&self.settings).clone();
            self.stop_locked(&mut running).await;
            match self.listen(&settings).await {
                Ok(started) => {
                    *lock(&self.listening) = Some(started.addr);
                    *running = Some(started);
                }
                Err(why) => *lock(&self.listen_error) = Some(why),
            }
        }
        Ok(())
    }

    /// Close the listener and every connection; withdraw the pairing code.
    async fn stop(&self) {
        let mut running = self.running.lock().await;
        self.stop_locked(&mut running).await;
    }

    async fn stop_locked(&self, running: &mut Option<Running>) {
        *lock(&self.listening) = None;
        self.pairing.cancel();
        self.requests.cancel();
        if let Some(running) = running.take() {
            running.cancel.cancel();
            running.hub.shutdown();
            // The serve task drops the listener and aborts its connections
            // before it ends, so the port is free once this returns.
            let _ = running.task.await;
            tracing::debug!(addr = %running.addr, "remote control stopped listening");
        }
    }

    /// Publish the current status, if it changed.
    pub(crate) fn publish(&self) {
        let settings = lock(&self.settings).clone();
        let listening = *lock(&self.listening);
        let error = lock(&self.settings_error)
            .clone()
            .or_else(|| lock(&self.listen_error).clone());
        let status = RemoteStatus {
            enabled: settings.enabled,
            reach: settings.reach,
            address: settings.address.map(|ip| ip.to_string()),
            port: settings.port,
            listening,
            error,
            fingerprint: lock(&self.identity)
                .as_ref()
                .map(|identity| identity.fingerprint().display_groups()),
            device: self.store.device(),
            connected: listening.is_some() && self.streams.load(Ordering::SeqCst) > 0,
            pairing_expires_at: self.pairing.active().map(|ticket| ticket.expires_at),
            pairing_error: lock(&self.pairing_error).clone(),
            pairing_request: self.requests.view(),
            suggested_address: self.suggested_address.clone(),
        };
        self.status.send_if_modified(|current| {
            if *current == status {
                false
            } else {
                *current = status;
                true
            }
        });
    }

    /// Publish again after `after`, for states that change by the clock
    /// (a code or a pairing request running out).
    pub(crate) fn publish_later(self: &Arc<Self>, after: Duration) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::time::sleep(after).await;
            if let Some(inner) = weak.upgrade() {
                inner.publish();
            }
        });
    }

    /// End every open event stream.
    pub(crate) fn end_streams(&self) {
        self.generation.send_modify(|generation| *generation += 1);
    }

    pub(crate) fn generation(&self) -> watch::Receiver<u64> {
        self.generation.subscribe()
    }

    /// Take an event-stream place, if one is free.
    pub(crate) fn open_stream(self: &Arc<Self>) -> Option<StreamGuard> {
        let max = self.limits.max_streams;
        self.streams
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |open| {
                (open < max).then_some(open + 1)
            })
            .ok()?;
        self.publish();
        Some(StreamGuard {
            inner: self.clone(),
        })
    }

    /// Spend one of the device's actions for this minute, or say how long
    /// until one is free.
    pub(crate) fn take_action(&self) -> Result<(), Duration> {
        let window = Duration::from_secs(60);
        let now = self.clock.now();
        let mut actions = lock(&self.actions);
        while actions
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= window)
        {
            actions.pop_front();
        }
        if actions.len() >= self.limits.max_actions_per_minute {
            let oldest = actions.front().copied().unwrap_or(now);
            return Err(window.saturating_sub(now.saturating_duration_since(oldest)));
        }
        actions.push_back(now);
        Ok(())
    }

    /// Count a refused request; summarise at most once a minute.
    pub(crate) fn note_auth_failure(&self) {
        self.note_failure(&self.auth_failures, |count| Activity::AuthFailures {
            count,
        });
    }

    /// Count a refused pairing attempt; when it used the code up, say so.
    pub(crate) fn note_pairing_failure(&self, burnt: bool) {
        if burnt {
            *lock(&self.pairing_error) = Some(
                "The code stopped working after wrong codes were tried. Make a new one.".into(),
            );
        }
        self.note_failure(&self.pairing_failures, |count| Activity::PairingRefused {
            count,
        });
        self.publish();
    }

    fn note_failure(&self, failures: &Mutex<AuthFailures>, line: impl Fn(u64) -> Activity) {
        let mut failures = lock(failures);
        failures.pending += 1;
        let now = self.clock.now();
        let due = failures
            .written
            .is_none_or(|at| now.saturating_duration_since(at) >= AUTH_FAILURE_SUMMARY);
        if due {
            let count = std::mem::take(&mut failures.pending);
            failures.written = Some(now);
            let _ = self.activity.record(line(count));
        }
    }

    fn flush_auth_failures(&self) {
        for (failures, line) in [
            (
                &self.auth_failures,
                (|count| Activity::AuthFailures { count }) as fn(u64) -> Activity,
            ),
            (&self.pairing_failures, |count| Activity::PairingRefused {
                count,
            }),
        ] {
            let mut failures = lock(failures);
            if failures.pending > 0 {
                let count = std::mem::take(&mut failures.pending);
                failures.written = Some(self.clock.now());
                let _ = self.activity.record(line(count));
            }
        }
    }

    pub(crate) fn backend(&self) -> &Arc<dyn RemoteBackend> {
        &self.backend
    }
}

/// An open event stream. Dropping it frees the place and updates the
/// status's `connected`.
pub(crate) struct StreamGuard {
    inner: Arc<Inner>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.inner.streams.fetch_sub(1, Ordering::SeqCst);
        self.inner.publish();
    }
}
