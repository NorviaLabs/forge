//! `--self-test` (self-test builds only): one end-to-end journey in the real
//! webview, plus the security probes the page cannot fake.
//!
//! The page runs its half (`ui/src/selftest.js`, bundled only into the
//! self-test UI build) and reports by navigating to `/?selftest=<json>`.
//! There is no extra IPC command, so a self-test build has exactly the
//! release command surface. Rust then adds its own checks and decides.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::Deserialize;
use tauri::Listener;

use crate::bridge::DesktopEvent;
use crate::pinned::PINNED_CSP;

/// The scripted model behind the journey, one step per model request:
/// - the markdown prompt's answer;
/// - a gated `git reset --hard` the page approves once, then its answer;
/// - a gated `git clean -f` the page denies, then its answer;
/// - a model call held mid-stream until the page presses Stop.
///
/// It is strict: any request beyond these fails the turn.
pub const SCENARIO: &str = r#"{"strict": true, "steps": [
  {"text": {"text": "self-test **pong**", "chunks": ["self-test ", "**pong**"]}},
  {"tool_calls": {"calls": [{"id": "approve-me", "name": "git", "arguments": {"subcommand": "reset", "args": ["--hard", "HEAD"]}}]}},
  {"text": {"text": "after approve"}},
  {"tool_calls": {"calls": [{"id": "deny-me", "name": "git", "arguments": {"subcommand": "clean", "args": ["-f"]}}]}},
  {"text": {"text": "after deny"}},
  {"hold": {"id": "stop-me", "before": "working on it", "then": {"text": {"text": "held answer"}}}}
]}"#;

/// Untracked file in the self-test repository; only the denied
/// `git clean -f` would delete it.
pub const UNTRACKED: &str = "untracked.txt";

/// The answer the held model call would give if Stop did not cancel it.
const HELD_ANSWER: &str = "held answer";

/// The event the page tries to emit; the capability does not grant
/// `event:emit`, so Rust must never receive it.
pub const EMIT_PROBE_EVENT: &str = "forge-selftest-emit-probe";

#[derive(Default)]
pub struct Probe {
    repo: PathBuf,
    submitted: AtomicBool,
    completed_delivered: AtomicBool,
    emit_received: std::sync::Arc<AtomicBool>,
    egress_port: Option<u16>,
    decided: AtomicBool,
    /// What Rust itself delivered to the page.
    approvals_shown: Mutex<Vec<String>>,
    last_turn_state: Mutex<&'static str>,
    held_answer_delivered: AtomicBool,
}

/// What the page observed. Every field must be true to pass.
#[derive(Debug, Deserialize)]
struct PageResults {
    shell_survives_switch: bool,
    editor_navigation_and_reload: bool,
    #[serde(default)]
    editor_debug: String,
    theme_switches: bool,
    slash_command_ran: bool,
    optimistic_prompt_visible: bool,
    rendered_markdown: bool,
    unknown_command_blocked: bool,
    emit_blocked: bool,
    no_global_tauri: bool,
    prototype_frozen: bool,
    inline_script_blocked: bool,
    eval_blocked: bool,
    iframe_blocked: bool,
    egress_fetch_blocked: bool,
    egress_websocket_blocked: bool,
    approved_turn_completed: bool,
    denied_turn_completed: bool,
    stopped_during_hold: bool,
}

impl Probe {
    pub fn new(repo: PathBuf) -> Self {
        Self {
            repo,
            egress_port: listening_tcp()
                .ok()
                .and_then(|addresses| addresses.first().and_then(|a| port_of(a))),
            ..Default::default()
        }
    }

    pub fn hello(&self) -> serde_json::Value {
        serde_json::json!({
            "egress_port": self.egress_port,
            "emit_probe_event": EMIT_PROBE_EVENT,
        })
    }

    pub fn submitted(&self) {
        self.submitted.store(true, Ordering::SeqCst);
    }

    /// Record an event about to be delivered to the page.
    pub fn delivering(&self, event: &DesktopEvent) {
        let DesktopEvent::Session {
            turn_state,
            messages,
            pending_approval,
            ..
        } = event
        else {
            return;
        };
        if *turn_state == "completed" {
            self.completed_delivered.store(true, Ordering::SeqCst);
        }
        *self.last_turn_state.lock().unwrap() = turn_state;
        if let Some(approval) = pending_approval {
            let mut shown = self.approvals_shown.lock().unwrap();
            if !shown.contains(&approval.call_id) {
                shown.push(approval.call_id.clone());
            }
        }
        if messages.iter().any(|message| message.text == HELD_ANSWER) {
            self.held_answer_delivered.store(true, Ordering::SeqCst);
        }
    }

    /// Listen for the event the page must not be able to emit.
    pub fn watch_emit(&self, app: &tauri::AppHandle) {
        let flag = self.emit_received.clone();
        app.listen(EMIT_PROBE_EVENT, move |_| {
            flag.store(true, Ordering::SeqCst)
        });
    }

    /// Called on page load; acts only on the page's report URL.
    pub fn on_page_load(&self, app: &tauri::AppHandle, url: &tauri::Url) {
        let Some(report) = url
            .query_pairs()
            .find(|(key, _)| key == "selftest")
            .map(|(_, value)| value.into_owned())
        else {
            return;
        };
        if self.decided.swap(true, Ordering::SeqCst) {
            return;
        }
        let failures = self.failures(app, &report);
        if failures.is_empty() {
            println!("SELF_TEST PASS journey and security probes");
            app.exit(0);
        } else {
            for failure in &failures {
                eprintln!("SELF_TEST FAIL {failure}");
            }
            app.exit(1);
        }
    }

    fn failures(&self, app: &tauri::AppHandle, report: &str) -> Vec<String> {
        let mut failures = Vec::new();
        match serde_json::from_str::<PageResults>(report) {
            Ok(page) => {
                let checks = [
                    ("shell_survives_switch", page.shell_survives_switch),
                    (
                        "editor_navigation_and_reload",
                        page.editor_navigation_and_reload,
                    ),
                    ("theme_switches", page.theme_switches),
                    ("slash_command_ran", page.slash_command_ran),
                    (
                        "optimistic_prompt_visible",
                        page.optimistic_prompt_visible,
                    ),
                    ("rendered_markdown", page.rendered_markdown),
                    ("unknown_command_blocked", page.unknown_command_blocked),
                    ("emit_blocked", page.emit_blocked),
                    ("no_global_tauri", page.no_global_tauri),
                    ("prototype_frozen", page.prototype_frozen),
                    ("inline_script_blocked", page.inline_script_blocked),
                    ("eval_blocked", page.eval_blocked),
                    ("iframe_blocked", page.iframe_blocked),
                    ("egress_fetch_blocked", page.egress_fetch_blocked),
                    ("egress_websocket_blocked", page.egress_websocket_blocked),
                    ("approved_turn_completed", page.approved_turn_completed),
                    ("denied_turn_completed", page.denied_turn_completed),
                    ("stopped_during_hold", page.stopped_during_hold),
                ];
                for (name, passed) in checks {
                    if !passed {
                        failures.push(format!("page: {name}"));
                    }
                }
                if !page.editor_navigation_and_reload {
                    failures.push(format!("editor diagnostic: {}", page.editor_debug));
                }
            }
            Err(error) => failures.push(format!("unreadable page report: {error}")),
        }
        if !self.submitted.load(Ordering::SeqCst) {
            failures.push("the page never submitted a prompt through IPC".into());
        }
        if !self.completed_delivered.load(Ordering::SeqCst) {
            failures.push("no completed turn was delivered to the page".into());
        }
        if self.emit_received.load(Ordering::SeqCst) {
            failures.push("the page emitted an event it has no permission for".into());
        }
        failures.extend(self.journey_failures());
        failures.extend(config_failures(app));
        failures.extend(listener_failures());
        failures
    }
}

impl Probe {
    /// The approval journey's effects, checked on disk and against what
    /// Rust delivered, not taken from the page.
    fn journey_failures(&self) -> Vec<String> {
        let mut failures = Vec::new();
        let shown = self.approvals_shown.lock().unwrap().clone();
        if shown != ["approve-me", "deny-me"] {
            failures.push(format!(
                "expected approvals approve-me then deny-me, showed {shown:?}"
            ));
        }
        match std::fs::read_to_string(self.repo.join("README.md")) {
            Ok(text) if text == "self-test\n" => {}
            other => failures.push(format!(
                "the approved `git reset --hard` did not run: README.md is {other:?}"
            )),
        }
        if !self.repo.join(UNTRACKED).exists() {
            failures.push("the denied `git clean -f` ran: the untracked file is gone".into());
        }
        let last = *self.last_turn_state.lock().unwrap();
        if !matches!(last, "cancelled" | "interrupted") {
            failures.push(format!("the held turn ended {last:?}, not stopped"));
        }
        if self.held_answer_delivered.load(Ordering::SeqCst) {
            failures.push("a stopped turn still delivered the held answer".into());
        }
        failures
    }
}

/// The configuration actually embedded in this binary (a stale build can
/// diverge from the JSON the config tests read).
fn config_failures(app: &tauri::AppHandle) -> Vec<String> {
    let config = app.config();
    let security = &config.app.security;
    let mut failures = Vec::new();
    let csp = security.csp.as_ref().map(|csp| csp.to_string());
    if csp.as_deref() != Some(PINNED_CSP) {
        failures.push(format!("embedded CSP differs from the pinned one: {csp:?}"));
    }
    if !security.freeze_prototype {
        failures.push("embedded config: freezePrototype is off".into());
    }
    if config.app.with_global_tauri {
        failures.push("embedded config: withGlobalTauri is on".into());
    }
    if !matches!(
        security.pattern,
        tauri::utils::config::PatternKind::Isolation { .. }
    ) {
        failures.push("embedded config: the isolation pattern is off".into());
    }
    if config.identifier != "dev.forge.desktop.test" {
        failures.push(format!(
            "self-test build has the identity {}",
            config.identifier
        ));
    }
    failures
}

/// D00: the only listeners this process may hold are the session's
/// loopback egress proxy (TCP) and its egress Unix socket.
fn listener_failures() -> Vec<String> {
    let mut failures = tcp_listener_failures();
    match listening_unix() {
        Ok(paths) => {
            let egress = |path: &String| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("forge-egress-") && name.ends_with(".sock")
                    })
            };
            if paths.len() != 1 || !paths.iter().all(egress) {
                failures.push(format!(
                    "expected exactly the one egress Unix socket, found {paths:?}"
                ));
            }
        }
        Err(error) => failures.push(format!("could not list Unix sockets: {error}")),
    }
    failures
}

fn tcp_listener_failures() -> Vec<String> {
    match listening_tcp() {
        Ok(addresses) => {
            let mut failures: Vec<String> = addresses
                .iter()
                .filter(|address| !address.starts_with("127.0.0.1:"))
                .map(|address| format!("non-loopback listener {address}"))
                .collect();
            if addresses.len() != 1 {
                failures.push(format!(
                    "expected exactly the one egress listener, found {addresses:?}"
                ));
            }
            failures
        }
        Err(error) => vec![format!("could not list listeners: {error}")],
    }
}

/// This process's listening TCP sockets, as `host:port`, via `lsof`.
fn listening_tcp() -> Result<Vec<String>, String> {
    let output = std::process::Command::new("lsof")
        .args(["-nP", "-a", "-p"])
        .arg(std::process::id().to_string())
        .args(["-iTCP", "-sTCP:LISTEN", "-Fn"])
        .output()
        .map_err(|error| format!("lsof: {error}"))?;
    // `lsof` exits 1 when nothing matches.
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(str::to_string)
        .collect())
}

/// Filesystem paths of Unix sockets this process listens on.
#[cfg(target_os = "linux")]
fn listening_unix() -> Result<Vec<String>, String> {
    // `ss -xlpn`: listening Unix sockets with their owning processes.
    let output = std::process::Command::new("ss")
        .args(["-xlpn"])
        .output()
        .map_err(|error| format!("ss: {error}"))?;
    let me = format!("pid={},", std::process::id());
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter(|line| line.contains(&me))
        .filter_map(|line| line.split_whitespace().nth(4))
        .filter(|path| path.starts_with('/'))
        .map(str::to_string)
        .collect())
}

/// Filesystem paths of Unix sockets this process has bound. On macOS a
/// connected client socket has no path of its own in `lsof`; a bound
/// (listening) socket shows its path.
#[cfg(not(target_os = "linux"))]
fn listening_unix() -> Result<Vec<String>, String> {
    let output = std::process::Command::new("lsof")
        .args(["-nP", "-a", "-p"])
        .arg(std::process::id().to_string())
        .args(["-U", "-Fn"])
        .output()
        .map_err(|error| format!("lsof: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut paths: Vec<String> = text
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .filter(|name| name.starts_with('/'))
        .map(str::to_string)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn port_of(address: &str) -> Option<u16> {
    address.rsplit(':').next()?.parse().ok()
}
