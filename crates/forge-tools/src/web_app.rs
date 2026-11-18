//! The session's running web app, as `app_start` left it: what `web_check`
//! opens, and the app's recent output.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// Bytes of app output kept (the most recent).
pub const LOG_CAPACITY: usize = 64 * 1024;

/// The app's recent output, as lines without terminal colours.
#[derive(Debug, Default)]
pub struct AppLog {
    inner: Mutex<LogState>,
}

#[derive(Debug, Default)]
struct LogState {
    /// Complete, cleaned, non-blank lines, oldest first.
    lines: VecDeque<String>,
    /// Their bytes, each counted with its newline.
    bytes: usize,
    /// The line still being written, raw (a colour code may be split).
    partial: Vec<u8>,
}

impl LogState {
    fn finish_line(&mut self) {
        let raw = std::mem::take(&mut self.partial);
        let line = clean(&raw);
        if line.trim().is_empty() {
            return;
        }
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.bytes > LOG_CAPACITY {
            match self.lines.pop_front() {
                Some(old) => self.bytes -= old.len() + 1,
                None => break,
            }
        }
    }
}

/// A line as a person reads it: no terminal escapes, no control
/// characters but tabs, no trailing space.
fn clean(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                // CSI: parameters up to a final byte in @..~.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC (titles, links): up to BEL or ESC \.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' {
                            break;
                        }
                        if c == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out.trim_end().to_string()
}

impl AppLog {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LogState> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Add output as it arrives (any chunking).
    pub fn push(&self, bytes: &[u8]) {
        let mut state = self.lock();
        for &byte in bytes {
            if byte == b'\n' {
                state.finish_line();
            } else {
                state.partial.push(byte);
                // A line that never ends is cut, not kept whole.
                if state.partial.len() >= LOG_CAPACITY / 4 {
                    state.finish_line();
                }
            }
        }
    }

    /// The last `count` non-empty lines, oldest first. Copies only those.
    pub fn tail(&self, count: usize) -> Vec<String> {
        let state = self.lock();
        let partial = clean(&state.partial);
        let partial = (!partial.trim().is_empty()).then_some(partial);
        let from_lines = count.saturating_sub(usize::from(partial.is_some()));
        let skip = state.lines.len().saturating_sub(from_lines);
        let mut lines: Vec<String> = state.lines.iter().skip(skip).cloned().collect();
        lines.extend(partial.filter(|_| count > 0));
        lines
    }

    /// The most recent non-empty line (read every half second for Activity:
    /// one line, not the log).
    pub fn last_line(&self) -> Option<String> {
        self.tail(1).pop()
    }
}

/// A loopback port the app's output says it listens on, other than
/// `expected`: the hint when `app_start` waited on the wrong port. The
/// newest mention wins.
pub fn port_hint(lines: &[String], expected: u16) -> Option<u16> {
    const HOSTS: [&str; 5] = ["localhost:", "127.0.0.1:", "0.0.0.0:", "[::1]:", "[::]:"];
    lines.iter().rev().find_map(|line| {
        HOSTS.iter().find_map(|host| {
            line.match_indices(host).find_map(|(at, _)| {
                let digits: String = line[at + host.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                digits
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port != 0 && *port != expected)
            })
        })
    })
}

/// `app_start`'s arguments. The tool is declared with this schema and the
/// session parses calls with it, so the two cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppStartArgs {
    /// e.g. `npm run dev`
    pub command: String,
    /// Its port on 127.0.0.1 in the sandbox.
    pub port: u16,
}

/// `app_stop`'s arguments: none.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppStopArgs {}

/// The app `app_start` started and that still runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    /// Counts up per start, so a stale stop cannot clear a newer app.
    pub generation: u64,
    pub command: String,
    /// The port inside the sandbox.
    pub port: u16,
    /// Where the host reaches it (the ingress relay).
    pub origin: SocketAddr,
}

/// What an app needs to run.
#[derive(Clone)]
pub struct AppRunRequest {
    pub command: String,
    /// The port it listens on inside the sandbox.
    pub port: u16,
    pub workspace_root: std::path::PathBuf,
    /// The session's network, as for any confined command.
    pub egress: Option<Arc<crate::sandbox::EgressGrant>>,
    pub session_tmp: Option<std::path::PathBuf>,
    /// Where the relay inside the sandbox creates the app's socket.
    pub ingress_dir: std::path::PathBuf,
    pub credential_env_names: Arc<[String]>,
    /// The `app_start` call, for the egress audit trail.
    pub call_id: Option<String>,
    /// Where the app's output goes.
    pub log: Arc<AppLog>,
}

/// How an app run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppExit {
    /// The process exited by itself, with its code when it had one.
    Exited(Option<i32>),
    /// It never started; why.
    SpawnFailed(String),
}

/// Runs an app until it exits. Dropping the future stops the app, its
/// whole process tree included.
#[async_trait::async_trait]
pub trait AppRunner: Send + Sync {
    async fn run(&self, request: AppRunRequest) -> AppExit;
}

/// The real runner: the command confined like `bash`, with the ingress
/// relay that offers its port to the host.
#[derive(Debug, Default)]
pub struct SandboxedAppRunner;

#[async_trait::async_trait]
impl AppRunner for SandboxedAppRunner {
    async fn run(&self, request: AppRunRequest) -> AppExit {
        use tokio::io::AsyncReadExt;
        let spec =
            crate::shell_spawn::ShellSpawnSpec::build(crate::shell_spawn::ShellSpawnRequest {
                mode: crate::shell_spawn::ShellSpawnMode::Run,
                shell: "bash",
                command: &request.command,
                workspace_root: &request.workspace_root,
                egress: request.egress.as_deref(),
                session_tmp: request.session_tmp.as_deref(),
                confined: true,
                egress_label: request.call_id.as_deref(),
                credential_env_names: &request.credential_env_names,
                ingress: Some((&request.ingress_dir, request.port)),
            })
            .await;
        let mut spec = match spec {
            Ok(spec) => spec,
            Err(crate::ToolError::SandboxDenied { reason, .. }) => {
                return AppExit::SpawnFailed(format!("the sandbox is not available: {reason}"))
            }
            Err(error) => return AppExit::SpawnFailed(error.to_string()),
        };
        // The gateway (and the brokers it holds) must outlive the app.
        let _egress = spec.take_egress_invocation();
        let mut command = spec.tokio_command();
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::builtins::set_process_group(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return AppExit::SpawnFailed(error.to_string()),
        };
        // Dropping this run (Stop, a replacing app_start, session end) drops
        // the guard, which kills the whole tree: relay and app included.
        let mut group = crate::builtins::ProcessGroupGuard::new(child.id().map(|pid| pid as i32));
        let pump = |stream: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>,
                    log: Arc<AppLog>| async move {
            let Some(mut stream) = stream else {
                return;
            };
            let mut buf = [0u8; 8192];
            while let Ok(read) = stream.read(&mut buf).await {
                if read == 0 {
                    break;
                }
                log.push(&buf[..read]);
            }
        };
        let stdout = child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
        let stderr = child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>);
        // Readers run on their own: a child the app left behind can hold the
        // pipes open after the app itself has exited.
        let readers = [
            tokio::spawn(pump(stdout, request.log.clone())),
            tokio::spawn(pump(stderr, request.log.clone())),
        ];
        let status = child.wait().await;
        // The app is gone; anything it left behind goes with its group, and
        // its last words get a moment to arrive.
        group.kill();
        for reader in readers {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), reader).await;
        }
        match status {
            Ok(status) => AppExit::Exited(status.code()),
            Err(error) => AppExit::SpawnFailed(error.to_string()),
        }
    }
}

/// One session's running app, shared by the session and its tools.
#[derive(Debug)]
pub struct WebAppSlot {
    current: tokio::sync::watch::Sender<Option<RunningApp>>,
}

impl Default for WebAppSlot {
    fn default() -> Self {
        Self {
            current: tokio::sync::watch::Sender::new(None),
        }
    }
}

impl WebAppSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn get(&self) -> Option<RunningApp> {
        self.current.borrow().clone()
    }

    pub fn set(&self, app: RunningApp) {
        self.current.send_replace(Some(app));
    }

    /// Hear when the running app changes (started, replaced, gone).
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<RunningApp>> {
        self.current.subscribe()
    }

    /// Forget the app of `generation`; a newer app stays.
    pub fn clear(&self, generation: u64) {
        self.current.send_if_modified(|current| {
            let ours = current
                .as_ref()
                .is_some_and(|app| app.generation == generation);
            if ours {
                *current = None;
            }
            ours
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_keeps_the_last_lines() {
        let log = AppLog::default();
        for index in 0..100 {
            log.push(format!("line {index}\n").as_bytes());
        }
        assert_eq!(log.tail(3), ["line 97", "line 98", "line 99"]);
        assert_eq!(log.last_line().as_deref(), Some("line 99"));
    }

    #[test]
    fn lines_are_joined_across_chunks_and_blank_ones_skipped() {
        let log = AppLog::default();
        log.push(b"> acme@0.1.0 d");
        log.push(b"ev\n\n   \nVITE ready\r\n");
        log.push(b"partial");
        assert_eq!(log.tail(10), ["> acme@0.1.0 dev", "VITE ready", "partial"]);
        assert_eq!(log.last_line().as_deref(), Some("partial"));
    }

    #[test]
    fn terminal_colours_are_removed() {
        let log = AppLog::default();
        log.push(b"\x1b[32m\x1b[1mVITE\x1b[22m v5.4\x1b[39m  ready\n");
        assert_eq!(log.tail(1), ["VITE v5.4  ready"]);
    }

    #[test]
    fn the_log_is_bounded() {
        let log = AppLog::default();
        let line = "x".repeat(1000) + "\n";
        for _ in 0..1000 {
            log.push(line.as_bytes());
        }
        let kept: usize = log.tail(usize::MAX).iter().map(|l| l.len() + 1).sum();
        assert!(kept <= LOG_CAPACITY, "{kept}");
        assert!(kept >= LOG_CAPACITY / 2, "{kept}");
    }

    /// Activity reads the last line every half second while the app runs:
    /// that reads one line, not a copy of the whole log.
    #[test]
    fn the_last_line_is_cheap_on_a_full_log() {
        let log = AppLog::default();
        for index in 0..4_000 {
            log.push(format!("[vite] hmr update /src/c{index}.tsx\n").as_bytes());
        }
        let started = std::time::Instant::now();
        for _ in 0..1_000 {
            assert!(log.last_line().is_some());
        }
        let spent = started.elapsed();
        assert!(spent < std::time::Duration::from_millis(50), "{spent:?}");
        // And the same answers as before.
        assert_eq!(
            log.last_line().as_deref(),
            Some("[vite] hmr update /src/c3999.tsx")
        );
        log.push(b"partial");
        assert_eq!(log.last_line().as_deref(), Some("partial"));
        assert_eq!(log.tail(2), ["[vite] hmr update /src/c3999.tsx", "partial"]);
    }

    #[test]
    fn the_port_the_app_names_is_hinted() {
        let lines = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(
            port_hint(
                &lines("VITE ready\n  ➜  Local:   http://localhost:5174/"),
                5173
            ),
            Some(5174)
        );
        assert_eq!(
            port_hint(&lines("Listening on http://0.0.0.0:3000"), 5173),
            Some(3000)
        );
        assert_eq!(
            port_hint(&lines("ready on 127.0.0.1:8080 and [::1]:8080"), 5173),
            Some(8080)
        );
        assert_eq!(
            port_hint(&lines("Local: http://localhost:5173/"), 5173),
            None,
            "the expected port is no hint"
        );
        assert_eq!(port_hint(&lines("compiled in 300ms"), 5173), None);
        assert_eq!(
            port_hint(&lines("see https://example.com:8443/docs"), 5173),
            None,
            "not a local address"
        );
    }

    fn app(generation: u64) -> RunningApp {
        RunningApp {
            generation,
            command: "npm run dev".into(),
            port: 5173,
            origin: "127.0.0.1:43817".parse().unwrap(),
        }
    }

    #[test]
    fn the_slot_holds_the_running_app() {
        let slot = WebAppSlot::default();
        assert_eq!(slot.get(), None);
        slot.set(app(1));
        assert_eq!(slot.get(), Some(app(1)));
        slot.clear(1);
        assert_eq!(slot.get(), None);
    }

    /// A replaced app's stop arrives late: the newer app stays.
    #[test]
    fn a_stale_stop_keeps_the_newer_app() {
        let slot = WebAppSlot::default();
        slot.set(app(1));
        slot.set(app(2));
        slot.clear(1);
        assert_eq!(slot.get(), Some(app(2)));
    }

    fn request(
        workspace: &std::path::Path,
        ingress: &std::path::Path,
        command: &str,
        port: u16,
    ) -> AppRunRequest {
        AppRunRequest {
            command: command.into(),
            port,
            workspace_root: workspace.to_path_buf(),
            egress: None,
            session_tmp: None,
            ingress_dir: ingress.to_path_buf(),
            credential_env_names: Arc::from(Vec::<String>::new()),
            call_id: Some("app-start-1".into()),
            log: AppLog::new(),
        }
    }

    fn can_relay() -> bool {
        crate::sandbox::availability().is_ok() && crate::sandbox::relay_available()
    }

    /// Where the sandbox cannot relay an app out, nothing starts, and the
    /// reason names what is missing.
    #[tokio::test]
    async fn without_the_relay_the_app_does_not_start() {
        if can_relay() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let ingress = tempfile::tempdir().unwrap();
        let exit = SandboxedAppRunner
            .run(request(
                workspace.path(),
                ingress.path(),
                "echo started > started.txt",
                5173,
            ))
            .await;
        let AppExit::SpawnFailed(reason) = exit else {
            panic!("{exit:?}");
        };
        assert!(
            reason.contains("socat") || reason.contains("sandbox"),
            "{reason}"
        );
        assert!(
            !workspace.path().join("started.txt").exists(),
            "nothing ran"
        );
    }

    /// A confined app answers through its socket, its output reaches the
    /// log, and dropping the run stops it.
    #[tokio::test]
    async fn a_confined_app_is_served_through_its_socket_until_dropped() {
        use std::io::{Read, Write};
        if !can_relay() {
            return; // needs bubblewrap and socat (CI installs both)
        }
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("index.html"),
            "served from the sandbox",
        )
        .unwrap();
        let ingress = tempfile::tempdir().unwrap();
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let request = request(
            workspace.path(),
            ingress.path(),
            &format!("echo starting; exec python3 -m http.server {port} --bind 127.0.0.1"),
            port,
        );
        let log = request.log.clone();
        let run = tokio::spawn(async move { SandboxedAppRunner.run(request).await });
        let socket = ingress.path().join(forge_sandbox::INGRESS_SOCKET_NAME);
        let mut body = String::new();
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let Ok(mut stream) = crate::ingress::connect_app_socket(&socket) else {
                continue;
            };
            let _ = stream.write_all(b"GET /index.html HTTP/1.0\r\n\r\n");
            body.clear();
            let _ = stream.read_to_string(&mut body);
            if body.contains("served from the sandbox") {
                break;
            }
        }
        assert!(body.contains("served from the sandbox"), "{body}");
        assert!(
            log.tail(20).iter().any(|line| line == "starting"),
            "{:?}",
            log.tail(20)
        );
        run.abort();
        let _ = run.await;
        let mut stopped = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            let answered = crate::ingress::connect_app_socket(&socket).is_ok_and(|mut stream| {
                let _ = stream.write_all(b"GET / HTTP/1.0\r\n\r\n");
                let mut out = String::new();
                let _ = stream.read_to_string(&mut out);
                !out.is_empty()
            });
            if !answered {
                stopped = true;
                break;
            }
        }
        assert!(stopped, "the app must stop when its run is dropped");
    }

    #[tokio::test]
    async fn an_app_that_exits_reports_its_code_and_output() {
        if !can_relay() {
            return; // needs bubblewrap and socat (CI installs both)
        }
        let workspace = tempfile::tempdir().unwrap();
        let ingress = tempfile::tempdir().unwrap();
        let request = request(
            workspace.path(),
            ingress.path(),
            "echo 'Error: Cannot find module vite' >&2; exit 3",
            5173,
        );
        let log = request.log.clone();
        let exit = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            SandboxedAppRunner.run(request),
        )
        .await
        .unwrap();
        assert_eq!(exit, AppExit::Exited(Some(3)));
        assert_eq!(
            log.last_line().as_deref(),
            Some("Error: Cannot find module vite")
        );
    }

    /// A watcher hears a start, a replacement and a stop, and not a stale
    /// stop that changes nothing.
    #[tokio::test]
    async fn watchers_hear_the_app_change() {
        let slot = WebAppSlot::default();
        let mut changes = slot.subscribe();
        let wait = std::time::Duration::from_secs(1);
        slot.set(app(1));
        tokio::time::timeout(wait, changes.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*changes.borrow_and_update(), Some(app(1)));
        slot.clear(2);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), changes.changed())
                .await
                .is_err(),
            "a stale stop changes nothing"
        );
        slot.clear(1);
        tokio::time::timeout(wait, changes.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(*changes.borrow_and_update(), None);
    }

    #[test]
    fn a_new_context_has_no_app() {
        let ctx = crate::ToolContext::new(std::env::temp_dir());
        assert_eq!(ctx.web_app.get(), None);
    }
}
