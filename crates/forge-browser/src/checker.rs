//! The web check browser: a headless browser that opens pages of the
//! user's app and collects what went wrong on them.
//!
//! It is not the agent's browser. It starts with a fresh profile, reaches
//! only the app's origin (everything else meets a [`DenyProxy`] that refuses
//! it), and is driven by a fixed plan per page: open it, wait until it has
//! settled, run the steps, check the expectations, take a screenshot. What
//! it reports: the page's HTTP status, console errors, uncaught exceptions,
//! failed requests to the app, and the outside hosts the page tried.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cdp::{CdpConnection, CdpError, CdpEvent};
use crate::deny_proxy::{Blocked, DenyProxy};
use crate::keys::KeyPress;
use crate::launch::{BrowserProcess, LaunchOptions};

/// A screen size a page is checked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    pub mobile: bool,
}

pub const DESKTOP: Viewport = Viewport {
    name: "desktop",
    width: 1280,
    height: 800,
    mobile: false,
};

pub const MOBILE: Viewport = Viewport {
    name: "mobile",
    width: 390,
    height: 844,
    mobile: true,
};

/// Whether a check takes a screenshot of its page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screenshot {
    Never,
    Always,
    /// Only when the page did not pass.
    IfFailed,
}

// What should be true on the page. (Plain comments on the schema types
// below: their doc comments become descriptions sent with every request.)
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
// Described once, in web_check's description: `text` is visible, an
// element matching `selector` is visible, the title contains `title`.
pub struct Expect {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Exactly one of click, type, press.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Target to click.
    #[serde(default)]
    pub click: Option<String>,
    /// Field to type `text` into (cleared first).
    #[serde(default, rename = "type")]
    pub type_into: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    /// Key or chord, e.g. `Enter`, `ctrl+a`.
    #[serde(default)]
    pub press: Option<String>,
    /// Checked after this step.
    #[serde(default)]
    pub expect: Option<Expect>,
}

/// One page to check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageSpec {
    /// The path on the app, with any query: `/settings?tab=2`.
    pub path: String,
    pub expect: Option<Expect>,
    pub steps: Vec<Step>,
}

/// An expectation and whether it held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectResult {
    /// What was expected, for the report: `“Dark mode” visible`.
    pub what: String,
    pub ok: bool,
    /// Why not, when it did not hold.
    pub detail: Option<String>,
}

/// A step and how it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// The step, for the report: `click “Greet”`.
    pub what: String,
    /// Why it could not be done; `None` when it was.
    pub error: Option<String>,
    pub expectations: Vec<ExpectResult>,
}

/// What one page showed at one viewport.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageReport {
    pub path: String,
    pub viewport: &'static str,
    /// The main document's HTTP status, when it answered.
    pub status: Option<u16>,
    /// Why the page could not be opened at all.
    pub load_error: Option<String>,
    pub console_errors: Vec<String>,
    pub exceptions: Vec<String>,
    /// Requests to the app that failed or answered 4xx/5xx:
    /// `GET /api/x → 404`.
    pub failed_requests: Vec<String>,
    /// Outside hosts the page tried (refused; not counted as failures).
    pub blocked: Vec<Blocked>,
    pub expectations: Vec<ExpectResult>,
    pub steps: Vec<StepResult>,
    /// Steps not run because an earlier one could not be done.
    pub skipped_steps: usize,
    /// PNG (or JPEG when large) of the viewport, when asked for.
    pub screenshot: Option<Vec<u8>>,
}

impl PageReport {
    /// Whether nothing went wrong: the page opened and answered below 400,
    /// nothing was logged as an error or thrown, every request to the app
    /// worked, every step could be done and every expectation held.
    /// Blocked outside hosts do not count.
    pub fn passed(&self) -> bool {
        self.load_error.is_none()
            && self.status.is_some_and(|status| status < 400)
            && self.console_errors.is_empty()
            && self.exceptions.is_empty()
            && self.failed_requests.is_empty()
            && self.skipped_steps == 0
            && self.steps.iter().all(|step| {
                step.error.is_none() && step.expectations.iter().all(|expect| expect.ok)
            })
            && self.expectations.iter().all(|expect| expect.ok)
    }
}

/// What a page visit has seen so far, filled in by the event listener.
#[derive(Default)]
struct Seen {
    /// Requests by id: method, URL, resource type.
    requests: HashMap<String, (String, String, String)>,
    /// Requests not finished yet (long-lived streams left out).
    inflight: HashSet<String>,
    /// The main document's request.
    document: Option<String>,
    status: Option<u16>,
    document_error: Option<String>,
    console_errors: Vec<String>,
    exceptions: Vec<String>,
    failed_requests: Vec<String>,
    /// Counted beyond the kept ones.
    dropped: usize,
    load_fired: bool,
    /// Main-frame loads begun (an action that moves it on navigated).
    navigations: u64,
    /// Requests begun (an action that moves it on started one).
    requests_started: u64,
    /// The main frame's URL, as last navigated.
    url: String,
}

/// Most entries kept per list; the rest are counted.
const MAX_ENTRIES: usize = 20;
/// Longest message kept.
const MAX_MESSAGE: usize = 300;
/// How long a page has to load.
const LOAD_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for the page's requests to go quiet.
const QUIET_CAP: Duration = Duration::from_secs(5);
/// How long without a request in flight counts as quiet.
const QUIET_FOR: Duration = Duration::from_millis(500);
/// How long after a step a load or request it caused may take to begin.
const STEP_GRACE: Duration = Duration::from_millis(100);
/// How long an expectation is retried before it fails.
const EXPECT_PATIENCE: Duration = Duration::from_secs(2);

impl Seen {
    fn push(list: &mut Vec<String>, dropped: &mut usize, message: String) {
        if list.len() < MAX_ENTRIES {
            list.push(shorten(&message));
        } else {
            *dropped += 1;
        }
    }
}

fn shorten(message: &str) -> String {
    let message = message.trim();
    if message.chars().count() <= MAX_MESSAGE {
        return message.to_string();
    }
    let mut cut: String = message.chars().take(MAX_MESSAGE).collect();
    cut.push('…');
    cut
}

/// The path (with query) of `url` when it is on `origin`.
fn on_origin(url: &str, origin: SocketAddr) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("ws://"))?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if rest[..authority_end] != origin.to_string() {
        return None;
    }
    let path = &rest[authority_end..];
    let path = path.split('#').next().unwrap_or_default();
    Some(if path.is_empty() {
        "/".into()
    } else if path.starts_with('?') {
        format!("/{path}")
    } else {
        path.to_string()
    })
}

/// A console call's arguments as one line.
fn console_text(args: &Value) -> String {
    args.as_array()
        .into_iter()
        .flatten()
        .map(|arg| match &arg["value"] {
            Value::String(text) => text.clone(),
            Value::Null => arg["description"]
                .as_str()
                .or_else(|| arg["unserializableValue"].as_str())
                .unwrap_or_else(|| arg["type"].as_str().unwrap_or_default())
                .to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// An uncaught exception as one line, with where it was thrown.
fn exception_text(details: &Value, origin: SocketAddr) -> String {
    let description = details["exception"]["description"]
        .as_str()
        .or_else(|| details["exception"]["value"].as_str())
        .or_else(|| details["text"].as_str())
        .unwrap_or("uncaught exception");
    let first = description.lines().next().unwrap_or(description).trim();
    let url = details["url"].as_str().unwrap_or_default();
    let place = on_origin(url, origin).unwrap_or_else(|| url.to_string());
    match (place.is_empty(), details["lineNumber"].as_u64()) {
        (false, Some(line)) => format!(
            "{first} ({place}:{}:{})",
            line + 1,
            details["columnNumber"].as_u64().unwrap_or(0) + 1
        ),
        _ => first.to_string(),
    }
}

/// Follow what the page does, for as long as the browser runs.
async fn listen(
    cdp: CdpConnection,
    mut events: tokio::sync::broadcast::Receiver<CdpEvent>,
    session: String,
    main_frame: String,
    origin: SocketAddr,
    seen: Arc<Mutex<Seen>>,
) {
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        if event.session_id.as_deref() != Some(session.as_str()) {
            continue;
        }
        let params = &event.params;
        let mut seen = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let seen = &mut *seen;
        match event.method.as_str() {
            "Network.requestWillBeSent" => {
                let id = params["requestId"].as_str().unwrap_or_default().to_string();
                let kind = params["type"].as_str().unwrap_or_default().to_string();
                let method = params["request"]["method"]
                    .as_str()
                    .unwrap_or("GET")
                    .to_string();
                let url = params["request"]["url"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if kind == "Document" && params["frameId"].as_str() == Some(main_frame.as_str()) {
                    seen.document = Some(id.clone());
                }
                // A stream never finishes; waiting on it would wait the cap.
                if kind != "EventSource" {
                    seen.inflight.insert(id.clone());
                    seen.requests_started += 1;
                }
                seen.requests.insert(id, (method, url, kind));
            }
            "Network.responseReceived" => {
                let id = params["requestId"].as_str().unwrap_or_default();
                let status = params["response"]["status"].as_u64().unwrap_or(0) as u16;
                if seen.document.as_deref() == Some(id) {
                    seen.status = Some(status);
                } else if status >= 400 {
                    if let Some((method, url, _)) = seen.requests.get(id).cloned() {
                        if let Some(path) = on_origin(&url, origin) {
                            Seen::push(
                                &mut seen.failed_requests,
                                &mut seen.dropped,
                                format!("{method} {path} → {status}"),
                            );
                        }
                    }
                }
            }
            "Network.loadingFinished" => {
                let id = params["requestId"].as_str().unwrap_or_default();
                seen.inflight.remove(id);
            }
            "Network.loadingFailed" => {
                let id = params["requestId"].as_str().unwrap_or_default().to_string();
                seen.inflight.remove(&id);
                if params["canceled"] == true {
                    continue;
                }
                let error = params["errorText"].as_str().unwrap_or("failed").to_string();
                let reason = params["blockedReason"]
                    .as_str()
                    .map(|reason| format!(" ({reason})"))
                    .unwrap_or_default();
                if seen.document.as_deref() == Some(id.as_str()) {
                    seen.document_error = Some(format!("{error}{reason}"));
                } else if let Some((method, url, _)) = seen.requests.get(&id).cloned() {
                    // Outside hosts are the proxy's to report.
                    if let Some(path) = on_origin(&url, origin) {
                        Seen::push(
                            &mut seen.failed_requests,
                            &mut seen.dropped,
                            format!("{method} {path} → {error}{reason}"),
                        );
                    }
                }
            }
            "Runtime.consoleAPICalled" => {
                if matches!(params["type"].as_str(), Some("error" | "assert")) {
                    let text = console_text(&params["args"]);
                    Seen::push(&mut seen.console_errors, &mut seen.dropped, text);
                }
            }
            "Runtime.exceptionThrown" => {
                let text = exception_text(&params["exceptionDetails"], origin);
                Seen::push(&mut seen.exceptions, &mut seen.dropped, text);
            }
            "Log.entryAdded" => {
                let entry = &params["entry"];
                // Failed loads are reported as failed requests (or blocked
                // hosts), not twice as console lines.
                if entry["level"] == "error" && entry["source"] != "network" {
                    let text = entry["text"].as_str().unwrap_or_default().to_string();
                    Seen::push(&mut seen.console_errors, &mut seen.dropped, text);
                }
            }
            "Page.loadEventFired" => seen.load_fired = true,
            "Page.frameStartedLoading" => {
                if params["frameId"].as_str() == Some(main_frame.as_str()) {
                    seen.navigations += 1;
                }
            }
            "Page.frameNavigated" => {
                let frame = &params["frame"];
                if frame["parentId"].is_null() {
                    seen.url = frame["url"].as_str().unwrap_or_default().to_string();
                }
            }
            "Page.javascriptDialogOpening" => {
                // Alerts are acknowledged, anything asking is declined,
                // leaving a page is allowed: the check goes on either way.
                let accept = matches!(params["type"].as_str(), Some("alert" | "beforeunload"));
                let cdp = cdp.clone();
                let session = session.clone();
                tokio::spawn(async move {
                    let _ = cdp
                        .call_session(
                            &session,
                            "Page.handleJavaScriptDialog",
                            json!({"accept": accept}),
                        )
                        .await;
                });
            }
            _ => {}
        }
    }
}

/// The script the checks run in their own world on the page.
const CHECK_JS: &str = r#"(() => {
  if (window.__forgeCheck) return;
  const norm = (text) => String(text || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => {
    if (!el || !el.isConnected) return false;
    const style = getComputedStyle(el);
    if (style.visibility === 'hidden' || style.display === 'none') return false;
    const rect = el.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  };
  const names = (el) => [
    el.innerText, el.getAttribute('aria-label'), el.getAttribute('placeholder'),
    el.getAttribute('title'), el.getAttribute('alt'),
    (el.tagName === 'INPUT' && /^(submit|button|reset)$/i.test(el.type)) ? el.value : null,
    el.labels && el.labels.length ? Array.from(el.labels).map(l => l.innerText).join(' ') : null,
  ].filter(Boolean).map(name => norm(name).toLowerCase());
  const INTERACTIVE = 'button, a[href], input, select, textarea, summary, label, [role=button], ' +
    '[role=link], [role=tab], [role=menuitem], [role=checkbox], [role=switch], [role=option], ' +
    '[onclick], [tabindex], [contenteditable=""], [contenteditable=true]';
  const byText = (text) => {
    const want = norm(text).toLowerCase();
    if (!want) return null;
    let exact = null, partial = null;
    for (const el of document.querySelectorAll(INTERACTIVE)) {
      if (!visible(el)) continue;
      const found = names(el);
      if (found.some(name => name === want)) { exact = el; break; }
      if (!partial && found.some(name => name.includes(want))) partial = el;
    }
    let el = exact || partial;
    if (!el) {
      // Any visible element showing the text: the innermost one.
      let best = null;
      for (const candidate of document.body ? document.body.querySelectorAll('*') : []) {
        if (!visible(candidate)) continue;
        if (norm(candidate.innerText).toLowerCase().includes(want)) best = candidate;
      }
      el = best;
    }
    if (el && el.tagName === 'LABEL') {
      el = el.control || el.querySelector('input, select, textarea') || el;
    }
    return el;
  };
  const find = (target) => {
    if (target.startsWith('text=')) return byText(target.slice(5));
    return document.querySelector(target);
  };
  const describe = (el) => {
    const label = norm(el.innerText || el.getAttribute('aria-label') || el.getAttribute('placeholder') || el.value || '');
    return el.tagName.toLowerCase() + (label ? ' “' + label.slice(0, 60) + '”' : '');
  };
  window.__forgeCheck = {
    point(target) {
      const el = find(target);
      if (!el) return { missing: true };
      el.scrollIntoView({ block: 'center', inline: 'center' });
      if (!visible(el)) return { hidden: true, name: describe(el) };
      const rect = el.getBoundingClientRect();
      const x = rect.left + rect.width / 2, y = rect.top + rect.height / 2;
      const top = document.elementFromPoint(x, y);
      if (top && top !== el && !el.contains(top) && !top.contains(el)) {
        return { covered: describe(top), name: describe(el) };
      }
      return { x, y, name: describe(el) };
    },
    focus(target) {
      const el = find(target);
      if (!el) return { missing: true };
      el.scrollIntoView({ block: 'center' });
      el.focus();
      if (typeof el.select === 'function') el.select();
      else if (el.isContentEditable) document.execCommand('selectAll');
      return { focused: document.activeElement === el || el.contains(document.activeElement), name: describe(el) };
    },
    text(want) {
      const body = document.body ? document.body.innerText : '';
      return norm(body).toLowerCase().includes(norm(want).toLowerCase());
    },
    selector(selector) {
      const all = Array.from(document.querySelectorAll(selector));
      return { count: all.length, visible: all.some(visible) };
    },
    title() { return document.title; },
  };
})()"#;

/// A user agent like the desktop one, as a phone's.
fn mobile_user_agent(desktop: &str) -> String {
    let version = desktop
        .split_whitespace()
        .find_map(|part| {
            part.strip_prefix("HeadlessChrome/")
                .or_else(|| part.strip_prefix("Chrome/"))
        })
        .unwrap_or("120.0.0.0");
    format!(
        "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) \
         Chrome/{version} Mobile Safari/537.36"
    )
}

/// A running checker browser for one app.
pub struct CheckerBrowser {
    origin: SocketAddr,
    proxy: DenyProxy,
    process: BrowserProcess,
    cdp: CdpConnection,
    session: String,
    user_agent: String,
    seen: Arc<Mutex<Seen>>,
    listener: tokio::task::JoinHandle<()>,
    /// The check world and the document it belongs to.
    world: Option<(String, i64)>,
    _profile: tempfile::TempDir,
}

impl Drop for CheckerBrowser {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl CheckerBrowser {
    /// Start a headless browser that reaches only `origin`.
    pub async fn start(
        executable: &Path,
        origin: SocketAddr,
        extra_args: Vec<String>,
    ) -> Result<Self, String> {
        let proxy = DenyProxy::start()
            .await
            .map_err(|error| format!("could not start the check proxy: {error}"))?;
        let profile = tempfile::Builder::new()
            .prefix("forge-web-check-")
            .tempdir()
            .map_err(|error| format!("could not create the check profile: {error}"))?;
        let options = LaunchOptions {
            executable: executable.to_path_buf(),
            profile_dir: profile.path().to_path_buf(),
            proxy: Some(proxy.addr()),
            direct_origin: Some(origin),
            headless: true,
            extra_args,
        };
        let (mut process, cdp) = BrowserProcess::launch(&options)
            .await
            .map_err(|error| error.to_string())?;
        match Self::attach(&cdp, origin).await {
            Ok((session, user_agent, seen, listener)) => Ok(Self {
                origin,
                proxy,
                process,
                cdp,
                session,
                user_agent,
                seen,
                listener,
                world: None,
                _profile: profile,
            }),
            Err(error) => {
                process.kill().await;
                Err(error)
            }
        }
    }

    async fn attach(
        cdp: &CdpConnection,
        origin: SocketAddr,
    ) -> Result<
        (
            String,
            String,
            Arc<Mutex<Seen>>,
            tokio::task::JoinHandle<()>,
        ),
        String,
    > {
        let failed = |error: CdpError| format!("the check browser did not start: {error}");
        crate::page::deny_downloads(cdp).await?;
        let version = cdp
            .call("Browser.getVersion", json!({}))
            .await
            .map_err(failed)?;
        let user_agent = version["userAgent"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let target = crate::setup::ensure_page_target(cdp)
            .await
            .map_err(failed)?;
        let session = crate::setup::attach_target(cdp, &target)
            .await
            .map_err(failed)?;
        let seen = Arc::new(Mutex::new(Seen::default()));
        // Subscribed before the domains are on, so nothing is missed.
        let listener = tokio::spawn(listen(
            cdp.clone(),
            cdp.subscribe(),
            session.clone(),
            target.clone(),
            origin,
            seen.clone(),
        ));
        for domain in [
            "Page.enable",
            "Runtime.enable",
            "Network.enable",
            "Log.enable",
        ] {
            if let Err(error) = cdp.call_session(&session, domain, json!({})).await {
                listener.abort();
                return Err(failed(error));
            }
        }
        let tree = cdp
            .call_session(&session, "Page.getFrameTree", json!({}))
            .await
            .map_err(failed)?;
        let main_frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .unwrap_or(&target)
            .to_string();
        if main_frame != target {
            // The listener matched the document by the target id; start it
            // again with the frame's own.
            listener.abort();
            let listener = tokio::spawn(listen(
                cdp.clone(),
                cdp.subscribe(),
                session.clone(),
                main_frame.clone(),
                origin,
                seen.clone(),
            ));
            return Ok((session, user_agent, seen, listener));
        }
        Ok((session, user_agent, seen, listener))
    }

    /// The app this browser checks.
    pub fn origin(&self) -> SocketAddr {
        self.origin
    }

    pub fn is_alive(&self) -> bool {
        !self.cdp.is_closed()
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.cdp.call_session(&self.session, method, params).await
    }

    async fn apply_viewport(&self, viewport: Viewport) -> Result<(), CdpError> {
        self.call(
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": viewport.width,
                "height": viewport.height,
                "deviceScaleFactor": 1,
                "mobile": viewport.mobile,
            }),
        )
        .await?;
        self.call(
            "Emulation.setTouchEmulationEnabled",
            json!({"enabled": viewport.mobile, "maxTouchPoints": if viewport.mobile { 5 } else { 1 }}),
        )
        .await?;
        let user_agent = if viewport.mobile {
            mobile_user_agent(&self.user_agent)
        } else {
            self.user_agent.clone()
        };
        self.call(
            "Emulation.setUserAgentOverride",
            json!({"userAgent": user_agent}),
        )
        .await?;
        Ok(())
    }

    /// Wait until the page has loaded, then until its requests go quiet.
    async fn settle(&self, load_timeout: Duration) {
        let deadline = Instant::now() + load_timeout;
        while Instant::now() < deadline {
            if self.seen().load_fired {
                break;
            }
            if let Ok(state) = self
                .call(
                    "Runtime.evaluate",
                    json!({"expression": "document.readyState", "returnByValue": true}),
                )
                .await
            {
                if state["result"]["value"] == "complete" {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.quiet(QUIET_CAP).await;
    }

    /// Wait until no request has been in flight for a moment (at most `cap`).
    async fn quiet(&self, cap: Duration) {
        let deadline = Instant::now() + cap;
        let mut quiet_since: Option<Instant> = None;
        while Instant::now() < deadline {
            if self.seen().inflight.is_empty() {
                let since = *quiet_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= QUIET_FOR {
                    return;
                }
            } else {
                quiet_since = None;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// After a step: if it started a page load, wait for it and then for
    /// quiet; if it started requests, wait for quiet; otherwise go on at once
    /// (expectations retry for a moment, which covers a late DOM change).
    async fn settle_after(&self, navigations_before: u64, requests_before: u64) {
        tokio::time::sleep(STEP_GRACE).await;
        let (navigations, requests, inflight) = {
            let seen = self.seen();
            (
                seen.navigations,
                seen.requests_started,
                !seen.inflight.is_empty(),
            )
        };
        if navigations != navigations_before {
            self.seen().load_fired = false;
            self.settle(LOAD_TIMEOUT).await;
        } else if requests != requests_before || inflight {
            self.quiet(QUIET_CAP).await;
        }
    }

    /// The check world on the current document, with the script in it.
    async fn world(&mut self) -> Result<i64, String> {
        let tree = self
            .call("Page.getFrameTree", json!({}))
            .await
            .map_err(|error| error.to_string())?;
        let frame = &tree["frameTree"]["frame"];
        let loader = frame["loaderId"].as_str().unwrap_or_default().to_string();
        if let Some((known, context)) = &self.world {
            if *known == loader {
                return Ok(*context);
            }
        }
        let created = self
            .call(
                "Page.createIsolatedWorld",
                json!({"frameId": frame["id"], "worldName": "forge-check"}),
            )
            .await
            .map_err(|error| error.to_string())?;
        let context = created["executionContextId"]
            .as_i64()
            .ok_or("the page has no room for the check")?;
        self.evaluate_in(context, CHECK_JS).await?;
        self.world = Some((loader, context));
        Ok(context)
    }

    async fn evaluate_in(&self, context: i64, expression: &str) -> Result<Value, String> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": context,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await
            .map_err(|error| error.to_string())?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("script error");
            return Err(text.lines().next().unwrap_or(text).to_string());
        }
        Ok(result["result"]["value"].clone())
    }

    /// Run `expression` with the check script; a document replaced in
    /// between gets a fresh world, once.
    async fn script(&mut self, expression: &str) -> Result<Value, String> {
        for attempt in 0..2 {
            let context = self.world().await?;
            match self.evaluate_in(context, expression).await {
                Err(message)
                    if attempt == 0
                        && (message.contains("Cannot find context")
                            || message.contains("__forgeCheck")) =>
                {
                    self.world = None;
                }
                other => return other,
            }
        }
        Err("the page kept changing".into())
    }

    async fn mouse(&self, kind: &str, x: f64, y: f64, clicks: u32) -> Result<(), CdpError> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind,
                "x": x,
                "y": y,
                "button": if kind == "mouseMoved" { "none" } else { "left" },
                "buttons": if kind == "mousePressed" { 1 } else { 0 },
                "clickCount": clicks,
            }),
        )
        .await
        .map(drop)
    }

    async fn key(&self, press: &KeyPress) -> Result<(), CdpError> {
        let mut down = json!({
            "type": if press.text.is_some() { "keyDown" } else { "rawKeyDown" },
            "key": press.key,
            "code": press.code,
            "windowsVirtualKeyCode": press.key_code,
            "modifiers": press.modifiers,
        });
        if let Some(text) = &press.text {
            down["text"] = json!(text);
            down["unmodifiedText"] = json!(text);
        }
        self.call("Input.dispatchKeyEvent", down).await?;
        self.call(
            "Input.dispatchKeyEvent",
            json!({
                "type": "keyUp",
                "key": press.key,
                "code": press.code,
                "windowsVirtualKeyCode": press.key_code,
                "modifiers": press.modifiers,
            }),
        )
        .await
        .map(drop)
    }

    /// Do one step; `Err` says why it could not be done.
    async fn run_step(&mut self, step: &Step) -> Result<(), String> {
        let missing = |target: &str| format!("no element matches {target}");
        match (&step.click, &step.type_into, &step.press) {
            (Some(target), None, None) => {
                let at = self
                    .script(&format!("__forgeCheck.point({})", json!(target)))
                    .await?;
                if at["missing"] == true {
                    return Err(missing(target));
                }
                if let Some(cover) = at["covered"].as_str() {
                    return Err(format!(
                        "{target} is covered by {cover}, which would get the click"
                    ));
                }
                if at["hidden"] == true {
                    return Err(format!("{target} is not visible"));
                }
                let (x, y) = (
                    at["x"].as_f64().unwrap_or(0.0),
                    at["y"].as_f64().unwrap_or(0.0),
                );
                let cdp_error = |error: CdpError| error.to_string();
                self.mouse("mouseMoved", x, y, 0).await.map_err(cdp_error)?;
                self.mouse("mousePressed", x, y, 1)
                    .await
                    .map_err(cdp_error)?;
                self.mouse("mouseReleased", x, y, 1)
                    .await
                    .map_err(cdp_error)
            }
            (None, Some(target), None) => {
                let focused = self
                    .script(&format!("__forgeCheck.focus({})", json!(target)))
                    .await?;
                if focused["missing"] == true {
                    return Err(missing(target));
                }
                if focused["focused"] != true {
                    return Err(format!("{target} could not be focused"));
                }
                let text = step.text.as_deref().unwrap_or_default();
                if text.is_empty() {
                    let backspace = crate::keys::parse_chord("Backspace")?;
                    self.key(&backspace)
                        .await
                        .map_err(|error| error.to_string())
                } else {
                    self.call("Input.insertText", json!({"text": text}))
                        .await
                        .map(drop)
                        .map_err(|error| error.to_string())
                }
            }
            (None, None, Some(chord)) => {
                let press = crate::keys::parse_chord(chord)?;
                self.key(&press).await.map_err(|error| error.to_string())
            }
            _ => Err("a step does one of click, type and press".into()),
        }
    }

    /// Check `expect`, retrying for a moment so a change that is on its way
    /// can land.
    async fn evaluate_expect(&mut self, expect: &Expect) -> Vec<ExpectResult> {
        let mut results = Vec::new();
        if let Some(text) = &expect.text {
            let what = format!("“{text}” visible");
            let held = self
                .eventually(&format!("__forgeCheck.text({})", json!(text)), |value| {
                    *value == true
                })
                .await;
            results.push(match held {
                Ok(_) => ExpectResult::ok(what),
                Err(error) => ExpectResult::failed(what, error.unwrap_or("not on the page".into())),
            });
        }
        if let Some(selector) = &expect.selector {
            let what = format!("{selector} visible");
            let held = self
                .eventually(
                    &format!("__forgeCheck.selector({})", json!(selector)),
                    |value| value["visible"] == true,
                )
                .await;
            results.push(match held {
                Ok(_) => ExpectResult::ok(what),
                Err(Some(error)) => ExpectResult::failed(what, error),
                Err(None) => {
                    let found = self
                        .script(&format!("__forgeCheck.selector({})", json!(selector)))
                        .await
                        .ok()
                        .and_then(|value| value["count"].as_u64())
                        .unwrap_or(0);
                    let detail = if found == 0 {
                        "no element matches".to_string()
                    } else {
                        format!("{found} match, none visible")
                    };
                    ExpectResult::failed(what, detail)
                }
            });
        }
        if let Some(title) = &expect.title {
            let what = format!("title contains “{title}”");
            let want = title.to_lowercase();
            let held = self
                .eventually("__forgeCheck.title()", |value| {
                    value
                        .as_str()
                        .is_some_and(|actual| actual.to_lowercase().contains(&want))
                })
                .await;
            results.push(match held {
                Ok(_) => ExpectResult::ok(what),
                Err(Some(error)) => ExpectResult::failed(what, error),
                Err(None) => {
                    let actual = self
                        .script("__forgeCheck.title()")
                        .await
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_default();
                    ExpectResult::failed(what, format!("the title is “{actual}”"))
                }
            });
        }
        results
    }

    /// Evaluate until `holds`, for up to [`EXPECT_PATIENCE`]. `Err(Some)`
    /// when the script failed (a bad selector), `Err(None)` when it never
    /// held.
    async fn eventually(
        &mut self,
        expression: &str,
        holds: impl Fn(&Value) -> bool,
    ) -> Result<(), Option<String>> {
        let deadline = Instant::now() + EXPECT_PATIENCE;
        loop {
            match self.script(expression).await {
                Ok(value) if holds(&value) => return Ok(()),
                Ok(_) => {}
                Err(error) if error.contains("not a valid selector") => {
                    return Err(Some(error));
                }
                Err(_) => {}
            }
            if Instant::now() >= deadline {
                return Err(None);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Open `page` at `viewport`, run its steps and expectations.
    pub async fn check(
        &mut self,
        page: &PageSpec,
        viewport: Viewport,
        screenshot: Screenshot,
    ) -> PageReport {
        let mut report = PageReport {
            path: page.path.clone(),
            viewport: viewport.name,
            ..PageReport::default()
        };
        // A blank page first, so nothing of the last check reaches this one.
        let _ = self
            .call("Page.navigate", json!({"url": "about:blank"}))
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        *self.seen() = Seen::default();
        self.world = None;
        self.proxy.take_blocked();

        if let Err(error) = self.apply_viewport(viewport).await {
            report.load_error = Some(format!("could not size the page: {error}"));
            return report;
        }
        let url = format!("http://{}{}", self.origin, page.path);
        let navigated = self
            .cdp
            .clone()
            .with_timeout(LOAD_TIMEOUT)
            .call_session(&self.session, "Page.navigate", json!({"url": url}))
            .await;
        match navigated {
            Err(error) => report.load_error = Some(format!("could not open {url}: {error}")),
            Ok(result) => {
                if let Some(error) = result["errorText"].as_str().filter(|e| !e.is_empty()) {
                    report.load_error = Some(format!("could not open the page: {error}"));
                }
            }
        }
        if report.load_error.is_none() {
            self.settle(LOAD_TIMEOUT).await;
            for (index, step) in page.steps.iter().enumerate() {
                let (navigations, requests) = {
                    let seen = self.seen();
                    (seen.navigations, seen.requests_started)
                };
                let what = step_summary(step);
                match self.run_step(step).await {
                    Ok(()) => {
                        self.settle_after(navigations, requests).await;
                        let expectations = match &step.expect {
                            Some(expect) => self.evaluate_expect(expect).await,
                            None => Vec::new(),
                        };
                        report.steps.push(StepResult {
                            what,
                            error: None,
                            expectations,
                        });
                    }
                    Err(error) => {
                        report.steps.push(StepResult {
                            what,
                            error: Some(error),
                            expectations: Vec::new(),
                        });
                        report.skipped_steps = page.steps.len() - index - 1;
                        break;
                    }
                }
            }
            if let Some(expect) = &page.expect {
                report.expectations = self.evaluate_expect(expect).await;
            }
        }

        let blocked = self.proxy.take_blocked();
        let seen = std::mem::take(&mut *self.seen());
        report.status = seen.status;
        if report.load_error.is_none() {
            if let Some(error) = seen.document_error {
                report.load_error = Some(format!("could not open the page: {error}"));
            } else if !seen.url.is_empty()
                && !seen.url.starts_with("about:")
                && on_origin(&seen.url, self.origin).is_none()
            {
                report.load_error = Some(format!(
                    "the page went to {}, outside the app",
                    shorten(&seen.url)
                ));
            }
        }
        // A console line about a blocked host is that host's refusal.
        let about_blocked =
            |line: &String| blocked.iter().any(|host| line.contains(host.host.as_str()));
        report.console_errors = seen
            .console_errors
            .into_iter()
            .filter(|line| !about_blocked(line))
            .collect();
        report.exceptions = seen.exceptions;
        report.failed_requests = seen.failed_requests;
        report.blocked = blocked;
        // Last, once the evidence is complete: whether the page failed is
        // known, and the picture shows the page as it was judged.
        let take = match screenshot {
            Screenshot::Never => false,
            Screenshot::Always => true,
            Screenshot::IfFailed => !report.passed(),
        };
        if take && report.load_error.is_none() {
            report.screenshot = self.screenshot().await.ok();
        }
        report
    }

    /// The viewport as PNG (JPEG when too large).
    async fn screenshot(&self) -> Result<Vec<u8>, String> {
        let shot = self
            .call("Page.captureScreenshot", json!({"format": "png"}))
            .await
            .map_err(|error| error.to_string())?;
        let mut bytes = crate::page::decode_screenshot(&shot)?;
        if bytes.len() as u64 > forge_types::MAX_IMAGE_BYTES {
            let shot = self
                .call(
                    "Page.captureScreenshot",
                    json!({"format": "jpeg", "quality": 60}),
                )
                .await
                .map_err(|error| error.to_string())?;
            bytes = crate::page::decode_screenshot(&shot)?;
        }
        Ok(bytes)
    }

    /// Stop the browser.
    pub async fn kill(mut self) {
        self.listener.abort();
        self.process.kill().await;
    }
}

impl ExpectResult {
    fn ok(what: String) -> Self {
        Self {
            what,
            ok: true,
            detail: None,
        }
    }

    fn failed(what: String, detail: String) -> Self {
        Self {
            what,
            ok: false,
            detail: Some(detail),
        }
    }
}

/// A step as the report names it: `click “text=Greet”`.
fn step_summary(step: &Step) -> String {
    match (&step.click, &step.type_into, &step.press) {
        (Some(target), _, _) => format!("click {target}"),
        (_, Some(target), _) => format!(
            "type “{}” into {target}",
            step.text.as_deref().unwrap_or_default()
        ),
        (_, _, Some(chord)) => format!("press {chord}"),
        _ => "an empty step".into(),
    }
}
