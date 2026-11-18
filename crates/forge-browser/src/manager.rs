//! One browser per session, started on first use.
//!
//! The manager is shared by the browser tools of one opened session (and
//! its children, which get browsers of their own). It holds the live
//! `[browser]` settings, so turning browser control off in Settings takes
//! effect on the next request, and it reports each browser's status to the
//! desktop app's "Browser open" strip.

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_config::BrowserConfig;
use forge_tools::egress::EgressInvocation;
use forge_tools::{ToolContext, ToolError};
use forge_types::SessionId;
use serde::Serialize;

use crate::discovery::{discover, DiscoveryError, FoundBrowser};
use crate::page::BrowserSession;

/// How the manager starts browsers. Tests relax `require_egress` and add
/// switches for the machine they run on.
#[derive(Debug, Clone)]
pub struct ManagerOptions {
    /// Switches appended to every launch.
    pub extra_args: Vec<String>,
    /// Refuse to start a browser for a session without an egress gateway,
    /// rather than letting it reach the network unfiltered. Always on in
    /// Forge; only tests without a gateway turn it off.
    pub require_egress: bool,
    /// Where to look for browsers. `None`: the process `PATH`.
    pub path_var: Option<OsString>,
}

impl Default for ManagerOptions {
    fn default() -> Self {
        Self {
            extra_args: Vec::new(),
            require_egress: true,
            path_var: None,
        }
    }
}

/// What the desktop app shows about a session's browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowserStatus {
    /// Display name of the browser program ("Google Chrome").
    pub browser: String,
    /// The active tab's URL and title.
    pub url: String,
    pub title: String,
    pub tabs: usize,
}

/// The browsers of one opened session.
pub struct BrowserManager {
    config: Mutex<BrowserConfig>,
    options: ManagerOptions,
    /// Discovery is a `PATH` walk: remembered per setting for a while, as
    /// availability is asked on every model request.
    found: Mutex<Option<(BrowserConfig, Instant, Discovered)>>,
    sessions: Mutex<HashMap<SessionId, Slot>>,
    status: Mutex<HashMap<SessionId, (BrowserStatus, u32)>>,
    /// Where screenshots go: a private directory, never the session's
    /// temp dir (which sandboxed commands can write).
    pub(crate) shots: forge_tools::screenshots::ScreenshotStore,
}

type Slot = Arc<tokio::sync::Mutex<Option<BrowserSession>>>;

/// The browser to start, or why there is none.
type Discovered = Result<FoundBrowser, DiscoveryError>;

/// How long a discovery result is reused.
const DISCOVERY_TTL: Duration = Duration::from_secs(30);

impl BrowserManager {
    pub fn new(config: BrowserConfig, options: ManagerOptions) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            options,
            found: Mutex::new(None),
            sessions: Mutex::new(HashMap::new()),
            status: Mutex::new(HashMap::new()),
            shots: Default::default(),
        })
    }

    /// Replace the live settings (Settings saved). A browser already running
    /// keeps running until closed; turning the feature off closes nothing
    /// by itself but makes the tools unavailable.
    pub fn set_config(&self, config: BrowserConfig) {
        *lock(&self.config) = config;
    }

    pub fn config(&self) -> BrowserConfig {
        lock(&self.config).clone()
    }

    /// The browser that would be started.
    pub fn found(&self) -> Result<FoundBrowser, DiscoveryError> {
        let config = self.config();
        let mut cache = lock(&self.found);
        if let Some((cached_for, at, result)) = cache.as_ref() {
            if *cached_for == config && at.elapsed() < DISCOVERY_TTL {
                return result.clone();
            }
        }
        let path_var = self
            .options
            .path_var
            .clone()
            .or_else(|| std::env::var_os("PATH"));
        let result = discover(config.executable.as_deref(), path_var.as_deref());
        *cache = Some((config, Instant::now(), result.clone()));
        result
    }

    /// Turned on, and a browser to start was found.
    pub fn is_available(&self) -> bool {
        self.config().enabled && self.found().is_ok()
    }

    /// The session's browser, when one is running.
    pub fn status(&self, session: SessionId) -> Option<BrowserStatus> {
        lock(&self.status)
            .get(&session)
            .map(|(status, _)| status.clone())
    }

    /// The running browser's process id, when one is running.
    pub fn browser_pid(&self, session: SessionId) -> Option<u32> {
        lock(&self.status).get(&session).map(|(_, pid)| *pid)
    }

    /// Close the session's browser. `false` when none was running.
    pub async fn close(&self, session: SessionId) -> bool {
        let slot = lock(&self.sessions).remove(&session);
        lock(&self.status).remove(&session);
        let Some(slot) = slot else {
            return false;
        };
        let browser = slot.lock().await.take();
        match browser {
            Some(browser) => {
                browser.kill().await;
                true
            }
            None => false,
        }
    }

    /// Close every browser this manager started.
    pub async fn close_all(&self) {
        let sessions: Vec<SessionId> = lock(&self.sessions).keys().copied().collect();
        for session in sessions {
            self.close(session).await;
        }
    }

    fn slot(&self, session: SessionId) -> Slot {
        lock(&self.sessions).entry(session).or_default().clone()
    }

    /// Run `action` on the session's browser, starting it when needed.
    pub(crate) async fn with_browser<T, F>(
        &self,
        ctx: &ToolContext,
        action: F,
    ) -> Result<T, ToolError>
    where
        F: for<'a> FnOnce(
            &'a mut BrowserSession,
        ) -> futures::future::BoxFuture<'a, Result<T, ToolError>>,
    {
        let session = ctx.session_id.unwrap_or_default();
        let slot = self.slot(session);
        let mut guard = slot.lock().await;
        if guard.as_ref().is_some_and(|browser| !browser.is_alive()) {
            if let Some(dead) = guard.take() {
                dead.kill().await;
            }
        }
        if guard.is_none() {
            *guard = Some(self.start(ctx).await?);
        }
        let browser = guard.as_mut().expect("started above");
        browser.begin_call(ctx.tool_call_id.as_deref());
        let result = action(browser).await;
        browser.end_call();
        if browser.is_alive() {
            let status = self.status_of(browser).await;
            lock(&self.status).insert(session, (status, browser.pid()));
        } else {
            // The browser died under the call: start afresh next time.
            if let Some(dead) = guard.take() {
                dead.kill().await;
            }
            lock(&self.status).remove(&session);
        }
        result
    }

    async fn status_of(&self, browser: &BrowserSession) -> BrowserStatus {
        let tabs = browser.tab_infos().await.unwrap_or_default();
        let active = tabs
            .iter()
            .find(|tab| tab.active)
            .cloned()
            .unwrap_or_default();
        BrowserStatus {
            browser: browser.browser_name.clone(),
            url: active.url,
            title: active.title,
            tabs: tabs.len(),
        }
    }

    async fn start(&self, ctx: &ToolContext) -> Result<BrowserSession, ToolError> {
        let egress = match ctx.egress.as_deref() {
            Some(grant) => {
                EgressInvocation::start_tcp_labeled_without_asking(Some(grant), Some("browser"))
                    .await
                    .map_err(|error| {
                        ToolError::Execution(format!(
                            "could not start the browser's network gateway: {error}"
                        ))
                    })?
            }
            None => None,
        };
        if egress.is_none() && self.options.require_egress {
            return Err(ToolError::Execution(
                "the browser needs this session's network gateway, and it has none: browser \
                 control is not available to an agent without network access"
                    .into(),
            ));
        }
        let found = self
            .found()
            .map_err(|error| ToolError::Execution(error.to_string()))?;
        let config = self.config();
        BrowserSession::start(
            found.path,
            found.name,
            config.headless,
            self.options.extra_args.clone(),
            egress,
        )
        .await
        .map_err(ToolError::Execution)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
