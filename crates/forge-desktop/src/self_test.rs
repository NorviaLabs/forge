//! `--self-test` (self-test builds only): one end-to-end journey in the real
//! webview, plus the security probes the page cannot fake.
//!
//! The page runs its half (`ui/src/selftest.js`, bundled only into the
//! self-test UI build) and reports by navigating to `/?selftest=<json>`.
//! There is no extra IPC command, so a self-test build has exactly the
//! release command surface. Rust then adds its own checks and decides.

use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tauri::Listener;

use crate::pinned::PINNED_CSP;

/// The event the page tries to emit; the capability does not grant
/// `event:emit`, so Rust must never receive it.
pub const EMIT_PROBE_EVENT: &str = "forge-selftest-emit-probe";

#[derive(Default)]
pub struct Probe {
    submitted: AtomicBool,
    completed_delivered: AtomicBool,
    emit_received: std::sync::Arc<AtomicBool>,
    egress_port: Option<u16>,
    decided: AtomicBool,
}

/// What the page observed. Every field must be true to pass.
#[derive(Debug, Deserialize)]
struct PageResults {
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
}

impl Probe {
    pub fn new() -> Self {
        Self {
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

    pub fn completed_delivered(&self) {
        self.completed_delivered.store(true, Ordering::SeqCst);
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
                ];
                for (name, passed) in checks {
                    if !passed {
                        failures.push(format!("page: {name}"));
                    }
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
        failures.extend(config_failures(app));
        failures.extend(listener_failures());
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
