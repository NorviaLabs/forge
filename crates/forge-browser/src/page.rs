//! One session's browser: its tabs, and the actions the tools perform.
//!
//! Every action works on the active tab, waits for the page to settle, and
//! reports what happened beyond itself: dialogs that were dismissed, a new
//! tab it opened, hosts the network policy blocked.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forge_tools::egress::EgressInvocation;
use forge_types::{ConsentKind, ConsentRequest};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::cdp::{CdpConnection, CdpError, CdpEvent};
use crate::keys::KeyPress;
use crate::launch::{BrowserProcess, LaunchOptions};
use crate::snapshot::{
    site_of, Element, FormInfo, Snapshot, TabInfo, MAX_ELEMENTS, MAX_TEXT_CHARS, SNAPSHOT_JS,
};

/// Why an action did not happen.
#[derive(Debug)]
pub(crate) enum PageError {
    /// Refused by Forge's rules (submit by click, a password field).
    Refused(String),
    /// Could not be done (a stale ref, no such option).
    Failed(String),
    /// The network policy refused the page's host.
    Denied { host: String, detail: String },
    /// The user has to agree first.
    Consent(Box<ConsentRequest>),
    /// The browser itself failed; it is restarted on the next call.
    Browser(String),
}

impl From<CdpError> for PageError {
    fn from(error: CdpError) -> Self {
        match error {
            CdpError::Protocol { message, .. } => PageError::Failed(message),
            other => PageError::Browser(other.to_string()),
        }
    }
}

pub(crate) type PageResult<T> = Result<T, PageError>;

/// What a click lands on.
pub(crate) enum ClickTarget {
    Ref(String),
    Point { x: f64, y: f64 },
}

/// How long a navigation may take, including a held host question.
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to wait for a page to finish loading after an action.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long after an action a page load it caused may take to begin.
const LOAD_GRACE: Duration = Duration::from_millis(40);

struct World {
    context_id: i64,
    loader_id: String,
}

struct Tab {
    target_id: String,
    session_id: String,
    world: Option<World>,
}

#[derive(Default)]
struct PageEvents {
    dialogs: Vec<String>,
    /// Frames (target ids for main frames) that are loading.
    loading: HashSet<String>,
    /// Page loads begun so far, in any tab: an action that moves it on
    /// started a load.
    started: u64,
}

/// What an action reports besides the page.
#[derive(Default)]
pub(crate) struct Notes {
    /// Forge's own notes (a new tab, hosts the policy blocked).
    pub lines: Vec<String>,
    /// Notes quoting the page (a dialog's message): shown with page content.
    pub page_lines: Vec<String>,
}

impl Notes {
    fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

/// The frame of the element the target script describes.
#[derive(Debug, Deserialize)]
struct TargetInfo {
    #[serde(default)]
    missing: bool,
    #[serde(flatten)]
    element: Element,
    #[serde(default)]
    point: Option<Point>,
    #[serde(default)]
    visible: bool,
    /// What is on top of the element's centre, when it is not the element.
    #[serde(default)]
    covered: Option<Element>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct Point {
    x: f64,
    y: f64,
}

pub(crate) struct BrowserSession {
    process: BrowserProcess,
    cdp: CdpConnection,
    pub(crate) browser_name: String,
    tabs: Vec<Tab>,
    active: usize,
    events: Arc<Mutex<PageEvents>>,
    listener: tokio::task::JoinHandle<()>,
    egress: Option<EgressInvocation>,
    /// Hosts refused during the current call, collected from the gateway.
    blocked: Vec<String>,
    /// Page loads begun before the current call (see [`Self::settle`]).
    mark: u64,
    _profile: tempfile::TempDir,
}

impl Drop for BrowserSession {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl BrowserSession {
    pub(crate) async fn start(
        executable: std::path::PathBuf,
        browser_name: String,
        headless: bool,
        extra_args: Vec<String>,
        egress: Option<EgressInvocation>,
    ) -> Result<Self, String> {
        let profile = tempfile::Builder::new()
            .prefix("forge-browser-")
            .tempdir()
            .map_err(|error| format!("could not create the browser profile: {error}"))?;
        let options = LaunchOptions {
            executable,
            profile_dir: profile.path().to_path_buf(),
            proxy: egress.as_ref().map(EgressInvocation::tcp_addr),
            direct_origin: None,
            headless,
            extra_args,
        };
        let (process, cdp) = BrowserProcess::launch(&options)
            .await
            .map_err(|error| error.to_string())?;
        let events = Arc::new(Mutex::new(PageEvents::default()));
        let listener = tokio::spawn(listen(cdp.clone(), cdp.subscribe(), events.clone()));
        // Downloads would write outside anything Forge tracks (a fresh
        // profile saves to ~/Downloads): a browser that keeps them on is not
        // used.
        if let Err(error) = deny_downloads(&cdp).await {
            listener.abort();
            let mut process = process;
            process.kill().await;
            return Err(error);
        }
        let mut session = Self {
            process,
            cdp,
            browser_name,
            tabs: Vec::new(),
            active: 0,
            events,
            listener,
            egress,
            blocked: Vec::new(),
            mark: 0,
            _profile: profile,
        };
        session
            .sync_tabs()
            .await
            .map_err(|error| format!("{error:?}"))?;
        if session.tabs.is_empty() {
            let created = session
                .cdp
                .call("Target.createTarget", json!({"url": "about:blank"}))
                .await
                .map_err(|error| error.to_string())?;
            let _ = created;
            session
                .sync_tabs()
                .await
                .map_err(|error| format!("{error:?}"))?;
        }
        Ok(session)
    }

    pub(crate) fn pid(&self) -> u32 {
        self.process.pid()
    }

    pub(crate) fn is_alive(&self) -> bool {
        !self.cdp.is_closed()
    }

    pub(crate) async fn kill(mut self) {
        self.listener.abort();
        self.process.kill().await;
    }

    /// Attribute the browser's connections to this tool call from now on.
    pub(crate) fn begin_call(&mut self, call_id: Option<&str>) {
        if let Some(egress) = &self.egress {
            egress.set_call_label(call_id);
            // Refusals before this call belong to no one now.
            egress.take_denied_host();
        }
        self.blocked.clear();
        let mut events = self.lock_events();
        events.dialogs.clear();
        let started = events.started;
        drop(events);
        self.mark = started;
    }

    pub(crate) fn end_call(&self) {
        if let Some(egress) = &self.egress {
            egress.set_call_label(None);
        }
    }

    fn lock_events(&self) -> std::sync::MutexGuard<'_, PageEvents> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn tab(&self) -> PageResult<&Tab> {
        self.tabs
            .get(self.active)
            .ok_or_else(|| PageError::Browser("the browser has no open tab".into()))
    }

    async fn call(&self, method: &str, params: Value) -> PageResult<Value> {
        let session = self.tab()?.session_id.clone();
        Ok(self.cdp.call_session(&session, method, params).await?)
    }

    /// Collect the hosts the gateway refused since the last look.
    fn collect_blocked(&mut self) {
        let Some(egress) = &self.egress else {
            return;
        };
        let hosts = egress.denied_hosts();
        egress.take_denied_host();
        for host in hosts {
            if !self.blocked.contains(&host) {
                self.blocked.push(host);
            }
        }
    }

    /// Dialogs dismissed, blocked hosts: what the page did besides.
    pub(crate) fn drain_notes(&mut self, notes: &mut Notes) {
        let dialogs = std::mem::take(&mut self.lock_events().dialogs);
        for message in dialogs {
            notes
                .page_lines
                .push(format!("A dialog \"{message}\" was dismissed."));
        }
        self.collect_blocked();
        let hosts = std::mem::take(&mut self.blocked);
        if !hosts.is_empty() {
            notes.push(format!(
                "Your network permissions blocked: {}. The page may look incomplete.",
                hosts.join(", ")
            ));
        }
    }

    /// Bring the tab list up to date: tabs the page opened are added (and
    /// become active), closed ones dropped. Returns how many were added.
    pub(crate) async fn sync_tabs(&mut self) -> PageResult<usize> {
        let pages = crate::setup::page_target_ids(&self.cdp).await?;
        let active_target = self.tabs.get(self.active).map(|tab| tab.target_id.clone());
        self.tabs.retain(|tab| pages.contains(&tab.target_id));
        let mut added = 0;
        for target in pages {
            if self.tabs.iter().any(|tab| tab.target_id == target) {
                continue;
            }
            let session_id = crate::setup::attach_target(&self.cdp, &target).await?;
            let _ = self
                .cdp
                .call_session(&session_id, "Page.enable", json!({}))
                .await;
            self.tabs.push(Tab {
                target_id: target,
                session_id,
                world: None,
            });
            added += 1;
        }
        // A tab the page just opened comes to the front, as in a browser.
        self.active = match active_target {
            Some(_) if added > 0 => self.tabs.len() - 1,
            Some(target) => self
                .tabs
                .iter()
                .position(|tab| tab.target_id == target)
                .unwrap_or(self.tabs.len().saturating_sub(1)),
            None => 0,
        };
        Ok(added)
    }

    /// Every tab with its title and URL.
    pub(crate) async fn tab_infos(&self) -> PageResult<Vec<TabInfo>> {
        let targets = self.cdp.call("Target.getTargets", json!({})).await?;
        let infos = targets["targetInfos"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(self
            .tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let info = infos
                    .iter()
                    .find(|info| info["targetId"] == tab.target_id.as_str());
                TabInfo {
                    title: info
                        .and_then(|info| info["title"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                    url: info
                        .and_then(|info| info["url"].as_str())
                        .unwrap_or_default()
                        .to_string(),
                    active: index == self.active,
                }
            })
            .collect())
    }

    /// The isolated world on the active tab's current document, with the
    /// snapshot script installed.
    async fn world(&mut self) -> PageResult<i64> {
        let tree = self.call("Page.getFrameTree", json!({})).await?;
        let frame = &tree["frameTree"]["frame"];
        let frame_id = frame["id"].as_str().unwrap_or_default().to_string();
        let loader_id = frame["loaderId"].as_str().unwrap_or_default().to_string();
        if let Some(world) = &self.tab()?.world {
            if world.loader_id == loader_id {
                return Ok(world.context_id);
            }
        }
        let created = self
            .call(
                "Page.createIsolatedWorld",
                json!({"frameId": frame_id, "worldName": "forge"}),
            )
            .await?;
        let context_id = created["executionContextId"]
            .as_i64()
            .ok_or_else(|| PageError::Browser("no isolated world".into()))?;
        self.evaluate_in(context_id, SNAPSHOT_JS).await?;
        self.tabs[self.active].world = Some(World {
            context_id,
            loader_id,
        });
        Ok(context_id)
    }

    async fn evaluate_in(&self, context_id: i64, expression: &str) -> PageResult<Value> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "contextId": context_id,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;
        if let Some(details) = result.get("exceptionDetails") {
            let text = details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("script error");
            return Err(PageError::Failed(text.to_string()));
        }
        Ok(result["result"]["value"].clone())
    }

    /// Run `expression` against Forge's script on the active document. A
    /// document replaced in between gets a fresh world, once.
    async fn forge(&mut self, expression: &str) -> PageResult<Value> {
        for attempt in 0..2 {
            let context = self.world().await?;
            match self.evaluate_in(context, expression).await {
                Err(PageError::Failed(message))
                    if attempt == 0
                        && (message.contains("Cannot find context")
                            || message.contains("__forge is not defined")) =>
                {
                    self.tabs[self.active].world = None;
                }
                other => return other,
            }
        }
        Err(PageError::Browser("the page kept changing".into()))
    }

    /// Wait for the active tab to finish loading what the action started.
    /// After an action: if it started a page load (within a short grace,
    /// as a click's navigation begins a moment later), wait for the load to
    /// finish; otherwise go on at once.
    pub(crate) async fn settle(&mut self) {
        let grace = Instant::now() + LOAD_GRACE;
        while self.lock_events().started == self.mark {
            if Instant::now() >= grace {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.wait_until_loaded().await;
    }

    /// Wait until the active tab has finished loading.
    pub(crate) async fn wait_until_loaded(&mut self) {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        while Instant::now() < deadline {
            let loading = match self.tab() {
                Ok(tab) => self.lock_events().loading.contains(&tab.target_id),
                Err(_) => false,
            };
            if !loading {
                let state = self
                    .call(
                        "Runtime.evaluate",
                        json!({"expression": "document.readyState", "returnByValue": true}),
                    )
                    .await;
                if let Ok(state) = state {
                    if state["result"]["value"] == "complete" {
                        return;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    pub(crate) async fn snapshot(&mut self) -> PageResult<Snapshot> {
        let value = self
            .forge(&format!(
                "__forge.snapshot({MAX_ELEMENTS}, {MAX_TEXT_CHARS})"
            ))
            .await?;
        let mut snapshot: Snapshot = serde_json::from_value(value)
            .map_err(|error| PageError::Browser(format!("unreadable snapshot: {error}")))?;
        snapshot.tabs = self.tab_infos().await?;
        Ok(snapshot)
    }

    /// Whether the page the tab now shows was itself refused by the policy:
    /// its host was blocked (the gateway answers plain HTTP with a 403 page
    /// and fails HTTPS, which shows Chrome's error page).
    pub(crate) async fn check_blocked(&mut self) -> PageResult<()> {
        self.collect_blocked();
        if self.blocked.is_empty() {
            return Ok(());
        }
        let url = self.current_url().await.unwrap_or_default();
        let page_host = site_of(&url).map(|site| host_part(&site));
        let host = match page_host {
            Some(host) if self.blocked.contains(&host) => Some(host),
            _ if url.starts_with("chrome-error://") => self.blocked.first().cloned(),
            _ => None,
        };
        match host {
            Some(host) => {
                self.blocked.retain(|blocked| *blocked != host);
                Err(PageError::Denied {
                    detail: format!("the browser could not load a page from {host}"),
                    host,
                })
            }
            None => Ok(()),
        }
    }

    async fn current_url(&self) -> PageResult<String> {
        let tree = self.call("Page.getFrameTree", json!({})).await?;
        Ok(tree["frameTree"]["frame"]["url"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    pub(crate) async fn navigate(&mut self, url: &str) -> PageResult<()> {
        let session = self.tab()?.session_id.clone();
        let result = self
            .cdp
            .clone()
            .with_timeout(NAVIGATION_TIMEOUT)
            .call_session(&session, "Page.navigate", json!({"url": url}))
            .await?;
        if let Some(error) = result["errorText"].as_str().filter(|e| !e.is_empty()) {
            self.collect_blocked();
            let wanted = site_of(url).map(|site| host_part(&site));
            let host = self
                .blocked
                .iter()
                .find(|host| Some(host.as_str()) == wanted.as_deref())
                .or(self.blocked.first())
                .cloned();
            if let Some(host) = host {
                self.blocked.retain(|blocked| *blocked != host);
                return Err(PageError::Denied {
                    detail: format!("could not load {url}: {error}"),
                    host,
                });
            }
            return Err(PageError::Failed(format!("could not load {url}: {error}")));
        }
        self.settle().await;
        self.check_blocked().await
    }

    pub(crate) async fn history(&mut self, action: &str) -> PageResult<()> {
        if action == "reload" {
            self.call("Page.reload", json!({})).await?;
        } else {
            let history = self.call("Page.getNavigationHistory", json!({})).await?;
            let index = history["currentIndex"].as_i64().unwrap_or(0);
            let entries = history["entries"].as_array().cloned().unwrap_or_default();
            let target = if action == "back" {
                index - 1
            } else {
                index + 1
            };
            let Some(entry) = usize::try_from(target).ok().and_then(|t| entries.get(t)) else {
                return Err(PageError::Failed(format!(
                    "there is no page to go {action} to"
                )));
            };
            self.call(
                "Page.navigateToHistoryEntry",
                json!({"entryId": entry["id"]}),
            )
            .await?;
        }
        self.settle().await;
        self.check_blocked().await
    }

    async fn target(&mut self, reference: &str) -> PageResult<(Element, Point)> {
        let (element, point, _) = self.target_uncovered(reference).await?;
        Ok((element, point))
    }

    /// The element, where it is, and what covers it there (if anything).
    async fn target_uncovered(
        &mut self,
        reference: &str,
    ) -> PageResult<(Element, Point, Option<Element>)> {
        let value = self
            .forge(&format!("__forge.target({})", json!(reference)))
            .await?;
        let info: TargetInfo = serde_json::from_value(value)
            .map_err(|error| PageError::Browser(format!("unreadable target: {error}")))?;
        if info.missing {
            return Err(stale(reference));
        }
        match (info.visible, info.point) {
            (true, Some(point)) => Ok((info.element, point, info.covered)),
            _ => Err(PageError::Failed(format!(
                "{reference} is not visible; scroll, or take a new browser_snapshot"
            ))),
        }
    }

    async fn mouse(&self, kind: &str, point: Point, button: &str, clicks: u32) -> PageResult<()> {
        self.call(
            "Input.dispatchMouseEvent",
            json!({
                "type": kind,
                "x": point.x,
                "y": point.y,
                "button": button,
                "buttons": if kind == "mousePressed" { button_bits(button) } else { 0 },
                "clickCount": clicks,
            }),
        )
        .await?;
        Ok(())
    }

    async fn click_at(&self, point: Point, button: &str, double: bool) -> PageResult<()> {
        self.mouse("mouseMoved", point, "none", 0).await?;
        self.mouse("mousePressed", point, button, 1).await?;
        self.mouse("mouseReleased", point, button, 1).await?;
        if double {
            self.mouse("mousePressed", point, button, 2).await?;
            self.mouse("mouseReleased", point, button, 2).await?;
        }
        Ok(())
    }

    /// Click, and return what was clicked (for the summary).
    pub(crate) async fn click(
        &mut self,
        target: &ClickTarget,
        button: &str,
        double: bool,
    ) -> PageResult<Option<Element>> {
        let (element, point) = match target {
            ClickTarget::Ref(reference) => {
                let (element, point, covered) = self.target_uncovered(reference).await?;
                if let Some(cover) = covered {
                    return Err(PageError::Refused(format!(
                        "{reference} is covered by {} \u{201c}{}\u{201d}, which would get the \
                         click instead; scroll, close what covers it, or look at a screenshot",
                        cover.role, cover.name
                    )));
                }
                (Some(element), point)
            }
            ClickTarget::Point { x, y } => {
                let at = self.forge(&format!("__forge.at({x}, {y})")).await?;
                let element: Option<Element> = serde_json::from_value(at).ok();
                (element, Point { x: *x, y: *y })
            }
        };
        if let Some(element) = element.as_ref().filter(|element| element.submits) {
            return Err(PageError::Refused(format!(
                "{} \u{201c}{}\u{201d} submits a form: use browser_submit, which asks the user first",
                element.reference, element.name
            )));
        }
        self.click_at(point, button, double).await?;
        Ok(element)
    }

    pub(crate) async fn type_text(
        &mut self,
        reference: &str,
        text: &str,
        clear: bool,
    ) -> PageResult<Element> {
        let (element, _) = self.target(reference).await?;
        if element.password {
            return Err(PageError::Refused(format!(
                "{reference} is a password field: ask the user to type it in the browser window"
            )));
        }
        if !matches!(
            element.role.as_str(),
            "textbox" | "combobox" | "searchbox" | "spinbutton"
        ) {
            return Err(PageError::Failed(format!(
                "{reference} ({} \u{201c}{}\u{201d}) does not take text",
                element.role, element.name
            )));
        }
        let focused = self
            .forge(&format!("__forge.focus({}, {clear})", json!(reference)))
            .await?;
        if focused["focused"] != true {
            return Err(PageError::Failed(format!(
                "{reference} could not be focused"
            )));
        }
        if text.is_empty() {
            if clear {
                self.key(&crate::keys::parse_chord("Backspace").expect("a named key"))
                    .await?;
            }
        } else {
            self.call("Input.insertText", json!({"text": text})).await?;
        }
        Ok(element)
    }

    async fn key(&self, press: &KeyPress) -> PageResult<()> {
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
        .await?;
        Ok(())
    }

    pub(crate) async fn press_key(&mut self, press: &KeyPress) -> PageResult<()> {
        // Typing a password key by key is typing a password.
        if press.text.is_some() {
            let focused = self.forge("__forge.focused()").await?;
            if focused["password"] == true {
                return Err(PageError::Refused(
                    "a password field has focus: ask the user to type the password in the \
                     browser window"
                        .into(),
                ));
            }
        }
        let submits_on_space = press.key == " " && press.modifiers == 0;
        if press.is_enter() || submits_on_space {
            let focused = self.forge("__forge.focused()").await?;
            let enter_submits = focused["enter_submits"] == true;
            let button_submits = focused["submits"] == true;
            if (press.is_enter() && enter_submits) || (submits_on_space && button_submits) {
                return Err(PageError::Refused(
                    "this key would submit a form: use browser_submit, which asks the user first"
                        .into(),
                ));
            }
        }
        self.key(press).await
    }

    pub(crate) async fn scroll(
        &mut self,
        reference: Option<&str>,
        dx: i64,
        dy: i64,
    ) -> PageResult<()> {
        let point = match reference {
            Some(reference) => self.target(reference).await?.1,
            None => Point {
                x: f64::from(crate::launch::WINDOW_WIDTH) / 2.0,
                y: f64::from(crate::launch::WINDOW_HEIGHT) / 2.0,
            },
        };
        self.mouse("mouseMoved", point, "none", 0).await?;
        self.call(
            "Input.dispatchMouseEvent",
            json!({"type": "mouseWheel", "x": point.x, "y": point.y, "deltaX": dx, "deltaY": dy}),
        )
        .await?;
        // Wait for the scroll (smooth or not) to come to rest.
        let mut last = None;
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            let position = self
                .call(
                    "Runtime.evaluate",
                    json!({"expression": "[scrollX, scrollY].join()", "returnByValue": true}),
                )
                .await?;
            let position = position["result"]["value"].as_str().map(str::to_string);
            if position.is_some() && position == last {
                break;
            }
            last = position;
        }
        Ok(())
    }

    pub(crate) async fn select(
        &mut self,
        reference: &str,
        value: &str,
    ) -> PageResult<(Element, String)> {
        let (element, _) = self.target(reference).await?;
        let result = self
            .forge(&format!(
                "__forge.select({}, {})",
                json!(reference),
                json!(value)
            ))
            .await?;
        if let Some(error) = result["error"].as_str() {
            let options: Vec<String> = result["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| o.as_str().map(str::to_string))
                .collect();
            return Err(PageError::Failed(if options.is_empty() {
                format!("{reference}: {error}")
            } else {
                format!(
                    "{reference}: {error}; the options are {}",
                    options.join(" | ")
                )
            }));
        }
        Ok((
            element,
            result["label"].as_str().unwrap_or(value).to_string(),
        ))
    }

    /// Submit the form `reference` is in, with the user's consent for its
    /// site.
    pub(crate) async fn submit(
        &mut self,
        reference: &str,
        consents: &forge_tools::SessionConsents,
        call_id: Option<&str>,
    ) -> PageResult<FormInfo> {
        let value = self
            .forge(&format!("__forge.form({})", json!(reference)))
            .await?;
        if value["missing"] == true {
            return Err(stale(reference));
        }
        if let Some(error) = value["error"].as_str() {
            return Err(PageError::Failed(format!("{reference}: {error}")));
        }
        let submitter = value["submitter_ref"].as_str().map(str::to_string);
        let form: FormInfo = serde_json::from_value(value)
            .map_err(|error| PageError::Browser(format!("unreadable form: {error}")))?;
        let (scope, _) = form.scope();
        if !consents.check(ConsentKind::FormSubmit, &scope, call_id) {
            let request = ConsentRequest::new(ConsentKind::FormSubmit, scope, form.summary())
                .with_reason(
                    "Submitting can create, send, buy or change something on the site, as you.",
                );
            return Err(PageError::Consent(Box::new(request)));
        }
        // A real click on the submit button, as the user would; otherwise
        // the form's own submit, with its validation and handlers.
        let clicked = match &submitter {
            Some(button) => match self.target_uncovered(button).await {
                Ok((_, point, None)) => {
                    self.click_at(point, "left", false).await?;
                    true
                }
                // Covered or hidden: submit the form itself instead.
                _ => false,
            },
            None => false,
        };
        if !clicked {
            self.forge(&format!("__forge.submit({})", json!(reference)))
                .await?;
        }
        self.settle().await;
        self.check_blocked().await?;
        Ok(form)
    }

    pub(crate) async fn activate(&mut self, index: usize) -> PageResult<()> {
        let tab = self
            .tabs
            .get(index)
            .ok_or_else(|| PageError::Failed(format!("there is no tab {index}")))?;
        self.cdp
            .call("Target.activateTarget", json!({"targetId": tab.target_id}))
            .await?;
        self.active = index;
        Ok(())
    }

    pub(crate) async fn close_tab(&mut self, index: usize) -> PageResult<()> {
        if self.tabs.len() <= 1 {
            return Err(PageError::Failed(
                "the last tab stays open; navigate it instead".into(),
            ));
        }
        let tab = self
            .tabs
            .get(index)
            .ok_or_else(|| PageError::Failed(format!("there is no tab {index}")))?;
        self.cdp
            .call("Target.closeTarget", json!({"targetId": tab.target_id}))
            .await?;
        let closed = tab.target_id.clone();
        let active = self.tabs[self.active].target_id.clone();
        self.tabs.retain(|tab| tab.target_id != closed);
        self.active = self
            .tabs
            .iter()
            .position(|tab| tab.target_id == active)
            .unwrap_or(self.tabs.len() - 1);
        Ok(())
    }

    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_index(&self) -> usize {
        self.active
    }

    /// PNG of the viewport (or the page, up to four screens tall).
    pub(crate) async fn screenshot(&mut self, full_page: bool) -> PageResult<Vec<u8>> {
        let mut params = json!({"format": "png"});
        if full_page {
            let metrics = self.call("Page.getLayoutMetrics", json!({})).await?;
            let size = &metrics["cssContentSize"];
            let width = size["width"]
                .as_f64()
                .unwrap_or(f64::from(crate::launch::WINDOW_WIDTH));
            let height = size["height"]
                .as_f64()
                .unwrap_or(f64::from(crate::launch::WINDOW_HEIGHT))
                .min(f64::from(crate::launch::WINDOW_HEIGHT) * 4.0);
            params["clip"] = json!({"x": 0, "y": 0, "width": width, "height": height, "scale": 1});
            params["captureBeyondViewport"] = json!(true);
        }
        let shot = self.call("Page.captureScreenshot", params.clone()).await?;
        let mut bytes = decode(&shot)?;
        if bytes.len() as u64 > forge_types::MAX_IMAGE_BYTES {
            params["format"] = json!("jpeg");
            params["quality"] = json!(60);
            let shot = self.call("Page.captureScreenshot", params).await?;
            bytes = decode(&shot)?;
        }
        Ok(bytes)
    }
}

/// Refuse every download, in every tab.
pub(crate) async fn deny_downloads(cdp: &CdpConnection) -> Result<(), String> {
    cdp.call("Browser.setDownloadBehavior", json!({"behavior": "deny"}))
        .await
        .map(drop)
        .map_err(|error| format!("the browser would not turn downloads off: {error}"))
}

/// A `Page.captureScreenshot` result's image bytes.
pub(crate) fn decode_screenshot(shot: &Value) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let data = shot["data"].as_str().unwrap_or_default();
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| format!("unreadable screenshot: {error}"))
}

fn decode(shot: &Value) -> PageResult<Vec<u8>> {
    use base64::Engine;
    let data = shot["data"].as_str().unwrap_or_default();
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|error| PageError::Browser(format!("unreadable screenshot: {error}")))
}

/// `host` of `host[:port]` (brackets kept for IPv6).
fn host_part(site: &str) -> String {
    match site.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
                && !host.ends_with(':') =>
        {
            host.to_string()
        }
        _ => site.to_string(),
    }
}

fn stale(reference: &str) -> PageError {
    PageError::Failed(format!(
        "there is no element {reference} on this page now; take a new browser_snapshot"
    ))
}

fn button_bits(button: &str) -> u32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        _ => 0,
    }
}

/// Answer dialogs and track loading, for as long as the browser runs.
async fn listen(
    cdp: CdpConnection,
    mut events: tokio::sync::broadcast::Receiver<CdpEvent>,
    state: Arc<Mutex<PageEvents>>,
) {
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        let lock = || {
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        };
        match event.method.as_str() {
            "Page.javascriptDialogOpening" => {
                let message = event.params["message"].as_str().unwrap_or_default();
                let kind = event.params["type"].as_str().unwrap_or_default();
                lock().dialogs.push(message.chars().take(200).collect());
                if let Some(session) = event.session_id.clone() {
                    let cdp = cdp.clone();
                    // Leaving a page is allowed; anything else is declined.
                    let accept = kind == "beforeunload";
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
            }
            "Page.frameStartedLoading" => {
                if let Some(frame) = event.params["frameId"].as_str() {
                    let mut state = lock();
                    state.loading.insert(frame.to_string());
                    state.started += 1;
                }
            }
            "Page.frameStoppedLoading" => {
                if let Some(frame) = event.params["frameId"].as_str() {
                    lock().loading.remove(frame);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A browser that refuses one method.
    fn refusing(method: &'static str) -> CdpConnection {
        let (ours_w, theirs_r) = tokio::io::duplex(1 << 16);
        let (theirs_w, ours_r) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let mut reader = BufReader::new(theirs_r);
            let mut writer = theirs_w;
            loop {
                let mut buf = Vec::new();
                if reader.read_until(0, &mut buf).await.unwrap_or(0) == 0 {
                    return;
                }
                buf.pop();
                let command: Value = serde_json::from_slice(&buf).unwrap();
                let reply = if command["method"] == method {
                    json!({"id": command["id"], "error": {"code": -32000, "message": "nope"}})
                } else {
                    json!({"id": command["id"], "result": {}})
                };
                let mut bytes = serde_json::to_vec(&reply).unwrap();
                bytes.push(0);
                writer.write_all(&bytes).await.unwrap();
            }
        });
        CdpConnection::spawn(ours_r, ours_w)
    }

    /// A fresh profile downloads to ~/Downloads: if downloads cannot be
    /// turned off, the browser is not used.
    #[tokio::test]
    async fn downloads_that_cannot_be_denied_stop_the_browser() {
        assert!(deny_downloads(&refusing("Browser.setDownloadBehavior"))
            .await
            .is_err());
        assert!(deny_downloads(&refusing("Other.method")).await.is_ok());
    }
}
