//! One running `claude` and the SDK's stdio control protocol.
//!
//! Lines on stdout are either control traffic or conversation messages.
//! Control responses complete Forge's own requests (`initialize`,
//! `set_model`, `interrupt`). Control requests come from Claude Code:
//! hook callbacks and MCP messages for Forge's in-process tool server.
//! Everything else goes to the conversation channel the current turn reads.
//!
//! A `tools/call` is held until Forge has run the tool: the result arrives
//! with the session's next model request and is released through
//! [`PendingTools::resolve`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use forge_connect::claude_agent::{self as claude, MCP_SERVER, TOOL_PREFIX};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

use crate::ModelError;

type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// How long Forge waits for Claude Code to answer a control request.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);
/// MCP protocol version Forge's tool server speaks.
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
/// Stderr kept for error messages.
const STDERR_TAIL: usize = 8 * 1024;

/// Which Claude conversation a process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionMode {
    /// A new saved conversation with this id.
    New(String),
    /// Continue a saved conversation.
    Resume(String),
    /// Nothing saved (side calls).
    Ephemeral,
}

/// Everything that needs a new process to change. The model changes on a
/// running process.
#[derive(Debug, Clone)]
pub(super) struct Launch {
    pub(super) cwd: PathBuf,
    pub(super) model: String,
    pub(super) effort: Option<String>,
    pub(super) system: String,
    pub(super) tools: Vec<Value>,
}

impl Launch {
    /// Settings whose change restarts the process.
    pub(super) fn key(&self) -> String {
        json!([self.cwd, self.effort, self.system, self.tools]).to_string()
    }

    fn args(&self, session: &SessionMode) -> Vec<String> {
        let mut args = vec![
            format!("--model={}", self.model),
            // Without summaries, Forge has no reasoning text to show.
            "--thinking=adaptive".into(),
            "--thinking-display=summarized".into(),
            "--include-partial-messages".into(),
        ];
        if let Some(effort) = &self.effort {
            args.push(format!("--effort={effort}"));
        }
        match session {
            SessionMode::New(id) => args.push(format!("--session-id={id}")),
            SessionMode::Resume(id) => args.push(format!("--resume={id}")),
            SessionMode::Ephemeral => args.push("--no-session-persistence".into()),
        }
        args
    }

    fn initialize(&self, request_id: &str) -> Value {
        let mut request = json!({
            "subtype": "initialize",
            // PreToolUse reports Claude's id for a call, in case a
            // `tools/call` arrives without it.
            "hooks": {"PreToolUse": [{"hookCallbackIds": ["pre_tool_use"]}]},
            "appendSystemPrompt": self.system,
            // Instruction changes must apply when a resumed conversation
            // continues.
            "systemPromptSnapshot": false,
            "skills": [],
        });
        if !self.tools.is_empty() {
            request["sdkMcpServers"] = json!([MCP_SERVER]);
            request["sdkMcpServerManifests"] = json!({
                MCP_SERVER: {
                    "initializeResult": server_info(MCP_PROTOCOL_VERSION),
                    "toolsListResult": {"tools": self.tools},
                }
            });
        }
        json!({"type": "control_request", "request_id": request_id, "request": request})
    }
}

fn server_info(protocol: &str) -> Value {
    json!({
        "protocolVersion": protocol,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": MCP_SERVER, "version": env!("CARGO_PKG_VERSION")},
    })
}

/// The MCP result Claude gets for a call Forge did not run.
pub(super) fn cancelled_result() -> Value {
    json!({"content": [{"type": "text", "text": "Cancelled by user."}], "isError": true})
}

enum Slot {
    Waiting(oneshot::Sender<Value>),
    Ready(Value),
}

/// Tool calls Claude is waiting on, keyed by Claude's tool-use id. A result
/// may arrive before or after Claude's `tools/call`.
#[derive(Default)]
pub(super) struct PendingTools {
    inner: StdMutex<PendingInner>,
}

#[derive(Default)]
struct PendingInner {
    slots: HashMap<String, Slot>,
    /// The id the latest PreToolUse hook reported.
    next: Option<String>,
}

impl PendingTools {
    fn lock(&self) -> std::sync::MutexGuard<'_, PendingInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn before(&self, id: &str) {
        self.lock().next = Some(id.to_string());
    }

    fn take_next(&self) -> Option<String> {
        self.lock().next.take()
    }

    fn wait(&self, id: &str) -> oneshot::Receiver<Value> {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.lock();
        match inner.slots.remove(id) {
            Some(Slot::Ready(value)) => {
                let _ = tx.send(value);
            }
            _ => {
                inner.slots.insert(id.to_string(), Slot::Waiting(tx));
            }
        }
        rx
    }

    /// Hand Claude the result of call `id`.
    pub(super) fn resolve(&self, id: &str, value: Value) {
        let mut inner = self.lock();
        match inner.slots.remove(id) {
            Some(Slot::Waiting(tx)) => {
                let _ = tx.send(value);
            }
            _ => {
                inner.slots.insert(id.to_string(), Slot::Ready(value));
            }
        }
    }

    /// Answer every held call as cancelled. Call only once the Claude turn
    /// is interrupted: an answer to a running turn starts another model
    /// call.
    pub(super) fn clear(&self) {
        let mut inner = self.lock();
        for (_, slot) in inner.slots.drain() {
            if let Slot::Waiting(tx) = slot {
                let _ = tx.send(cancelled_result());
            }
        }
        inner.next = None;
    }
}

/// State the reader task and the turn share.
pub(super) struct Shared {
    writer: Mutex<Writer>,
    controls: StdMutex<HashMap<String, oneshot::Sender<Result<Value, String>>>>,
    pub(super) tools: PendingTools,
    definitions: Vec<Value>,
    ended: AtomicBool,
    stderr: StdMutex<String>,
    next_request: AtomicU64,
}

impl Shared {
    async fn write(&self, value: &Value) -> Result<(), String> {
        let mut line = value.to_string();
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        writer.flush().await.map_err(|error| error.to_string())
    }

    /// Send a control request and wait for its response.
    async fn request(&self, request: Value) -> Result<Value, String> {
        let id = format!(
            "forge-{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        );
        self.request_with_id(
            &id,
            json!({"type": "control_request", "request_id": id, "request": request}),
        )
        .await
    }

    async fn request_with_id(&self, id: &str, message: Value) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        self.controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.to_string(), tx);
        self.write(&message).await?;
        match tokio::time::timeout(CONTROL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("Claude Code exited".into()),
            Err(_) => {
                self.controls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(id);
                Err(format!(
                    "Claude Code did not answer within {} seconds",
                    CONTROL_TIMEOUT.as_secs()
                ))
            }
        }
    }

    /// Interrupt the running turn, then cancel held tool calls.
    pub(super) async fn interrupt(&self) {
        let _ = self.request(json!({"subtype": "interrupt"})).await;
        self.tools.clear();
    }

    /// [`Self::interrupt`] from synchronous code (a dropped turn).
    fn interrupt_detached(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        let shared = Arc::clone(self);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => Some(handle.spawn(async move { shared.interrupt().await })),
            Err(_) => {
                self.tools.clear();
                None
            }
        }
    }

    pub(super) fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .trim()
            .to_string()
    }

    async fn respond(&self, request_id: &str, result: Result<Value, String>) {
        let response = match result {
            Ok(response) => json!({
                "type": "control_response",
                "response": {"subtype": "success", "request_id": request_id, "response": response},
            }),
            Err(error) => json!({
                "type": "control_response",
                "response": {"subtype": "error", "request_id": request_id, "error": error},
            }),
        };
        let _ = self.write(&response).await;
    }
}

/// A running `claude`.
pub(super) struct ClaudeProcess {
    pub(super) shared: Arc<Shared>,
    events: mpsc::UnboundedReceiver<Value>,
    child: Option<tokio::process::Child>,
    pub(super) launch_key: String,
    pub(super) model: String,
    /// A Claude turn is in progress.
    pub(super) running: bool,
    /// Interrupted turns whose `result` has not been read yet. Their
    /// trailing output is skipped.
    pub(super) stale_results: usize,
    /// An interrupt sent from a dropped turn. It must reach Claude before
    /// anything else is written.
    interrupting: Option<tokio::task::JoinHandle<()>>,
}

impl ClaudeProcess {
    /// Start `claude` and initialize it.
    pub(super) async fn spawn(launch: &Launch, session: &SessionMode) -> Result<Self, ModelError> {
        let executable = claude::executable()
            .ok_or_else(|| ModelError::Provider(claude::ProbeError::Missing.to_string()))?;
        let mut command = tokio::process::Command::from(claude::command(&executable, &launch.cwd));
        command
            .args(launch.args(session))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|error| {
            ModelError::Provider(format!("could not start Claude Code: {error}"))
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take();
        let mut process = Self::attach(launch, stdout, Box::new(stdin), Some(child));
        if let Some(stderr) = stderr {
            let shared = Arc::clone(&process.shared);
            tokio::spawn(collect_stderr(stderr, shared));
        }
        process.initialize(launch).await?;
        Ok(process)
    }

    /// Wrap an already-connected `claude` (tests drive one over pipes).
    pub(super) fn attach(
        launch: &Launch,
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: Writer,
        child: Option<tokio::process::Child>,
    ) -> Self {
        let shared = Arc::new(Shared {
            writer: Mutex::new(writer),
            controls: StdMutex::new(HashMap::new()),
            tools: PendingTools::default(),
            definitions: launch.tools.clone(),
            ended: AtomicBool::new(false),
            stderr: StdMutex::new(String::new()),
            next_request: AtomicU64::new(1),
        });
        let (events_tx, events) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(reader, Arc::clone(&shared), events_tx));
        Self {
            shared,
            events,
            child,
            launch_key: launch.key(),
            model: launch.model.clone(),
            running: false,
            stale_results: 0,
            interrupting: None,
        }
    }

    /// Wait for an interrupt a dropped turn sent.
    pub(super) async fn settle(&mut self) {
        if let Some(interrupt) = self.interrupting.take() {
            let _ = interrupt.await;
        }
    }

    pub(super) async fn initialize(&mut self, launch: &Launch) -> Result<(), ModelError> {
        let response = self
            .shared
            .request_with_id("forge-init", launch.initialize("forge-init"))
            .await
            .map_err(|error| self.failure(&format!("Claude Code failed to start: {error}")))?;
        let probe = claude::parse_initialize_response(&response);
        if !probe.logged_in {
            return Err(ModelError::ProviderAuth {
                status: 401,
                message: "Claude Code is not signed in. Run `claude auth login` in a terminal."
                    .into(),
            });
        }
        if let Some(reason) = claude::version_error(probe.version.as_deref()) {
            return Err(ModelError::Provider(reason));
        }
        Ok(())
    }

    fn failure(&self, message: &str) -> ModelError {
        let stderr = self.shared.stderr_tail();
        if stderr.is_empty() {
            ModelError::Provider(message.to_string())
        } else {
            ModelError::Provider(format!("{message}: {stderr}"))
        }
    }

    /// The process has exited or closed its output.
    pub(super) fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Relaxed)
    }

    pub(super) async fn set_model(&mut self, model: &str) -> Result<(), ModelError> {
        self.settle().await;
        self.shared
            .request(json!({"subtype": "set_model", "model": model}))
            .await
            .map_err(|error| {
                self.failure(&format!("Claude Code could not switch model: {error}"))
            })?;
        self.model = model.to_string();
        Ok(())
    }

    /// Start a Claude turn with `content` as the user message.
    pub(super) async fn send_user(&mut self, content: Vec<Value>) -> Result<(), ModelError> {
        self.settle().await;
        let message = json!({
            "type": "user",
            "session_id": "",
            "uuid": uuid::Uuid::new_v4().to_string(),
            "message": {"role": "user", "content": content},
            "parent_tool_use_id": null,
            // Forge already expanded commands and file references; Claude
            // Code must not process the text again.
            "client_composed": true,
        });
        self.shared
            .write(&message)
            .await
            .map_err(|error| self.failure(&format!("could not write to Claude Code: {error}")))?;
        self.running = true;
        Ok(())
    }

    /// Interrupt the running turn, if any, and cancel held tool calls.
    pub(super) async fn interrupt(&mut self) {
        self.settle().await;
        if self.running {
            self.running = false;
            self.stale_results += 1;
            self.shared.interrupt().await;
        } else {
            self.shared.tools.clear();
        }
    }

    /// Mark the running turn interrupted from synchronous code.
    pub(super) fn interrupt_detached(&mut self) {
        if self.running {
            self.running = false;
            self.stale_results += 1;
            self.interrupting = self.shared.interrupt_detached();
        } else {
            self.shared.tools.clear();
        }
    }

    /// The next conversation message, or `None` once `claude` has exited.
    pub(super) async fn next_event(&mut self) -> Option<Value> {
        self.events.recv().await
    }

    /// Why `claude` stopped, for an error.
    pub(super) fn exit_error(&self) -> ModelError {
        self.failure("Claude Code exited unexpectedly")
    }

    /// Close stdin and wait briefly, then kill.
    pub(super) async fn stop(mut self) {
        self.settle().await;
        self.shared.tools.clear();
        {
            let mut writer = self.shared.writer.lock().await;
            let _ = writer.shutdown().await;
        }
        if let Some(mut child) = self.child.take() {
            if tokio::time::timeout(Duration::from_secs(3), child.wait())
                .await
                .is_err()
            {
                let _ = child.kill().await;
            }
        }
    }
}

async fn collect_stderr(mut stderr: tokio::process::ChildStderr, shared: Arc<Shared>) {
    let mut buffer = [0u8; 4096];
    while let Ok(count) = stderr.read(&mut buffer).await {
        if count == 0 {
            break;
        }
        let mut tail = shared
            .stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tail.push_str(&String::from_utf8_lossy(&buffer[..count]));
        if tail.len() > STDERR_TAIL {
            let mut cut = tail.len() - STDERR_TAIL;
            while !tail.is_char_boundary(cut) {
                cut += 1;
            }
            tail.drain(..cut);
        }
    }
}

async fn read_loop(
    reader: impl AsyncRead + Unpin,
    shared: Arc<Shared>,
    events: mpsc::UnboundedSender<Value>,
) {
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match message.get("type").and_then(Value::as_str) {
            Some("control_response") => {
                let response = &message["response"];
                let id = response
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let waiter = shared
                    .controls
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(id);
                if let Some(waiter) = waiter {
                    let result = if response.get("subtype").and_then(Value::as_str) == Some("error")
                    {
                        Err(response
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string())
                    } else {
                        Ok(response.get("response").cloned().unwrap_or(Value::Null))
                    };
                    let _ = waiter.send(result);
                }
            }
            Some("control_request") => {
                let shared = Arc::clone(&shared);
                tokio::spawn(async move { handle_control(&shared, message).await });
            }
            Some("control_cancel_request" | "keep_alive") => {}
            _ => {
                let _ = events.send(message);
            }
        }
    }
    shared.ended.store(true, Ordering::Relaxed);
    // Fail every pending control request, and release held tool calls.
    shared
        .controls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    shared.tools.clear();
}

async fn handle_control(shared: &Shared, message: Value) {
    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let request = &message["request"];
    let result = match request.get("subtype").and_then(Value::as_str) {
        Some("hook_callback") => {
            let input = &request["input"];
            if input.get("hook_event_name").and_then(Value::as_str) == Some("PreToolUse") {
                if let Some(id) = input.get("tool_use_id").and_then(Value::as_str) {
                    shared.tools.before(id);
                }
            }
            Ok(json!({}))
        }
        Some("mcp_message") => {
            let server = request.get("server_name").and_then(Value::as_str);
            if server == Some(MCP_SERVER) {
                Ok(json!({"mcp_response": handle_mcp(shared, &request["message"]).await}))
            } else {
                Err(format!("unknown MCP server: {}", server.unwrap_or("")))
            }
        }
        // `dontAsk` with Forge's tools allowed should never ask; refuse
        // anything else.
        Some("can_use_tool") => {
            let tool = request
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if tool.starts_with(TOOL_PREFIX) {
                Ok(json!({"behavior": "allow", "updatedInput": request["input"]}))
            } else {
                Ok(json!({"behavior": "deny", "message": "Only Forge's tools are available."}))
            }
        }
        other => Err(format!(
            "Forge does not support the `{}` control request",
            other.unwrap_or("")
        )),
    };
    shared.respond(&request_id, result).await;
}

async fn handle_mcp(shared: &Shared, message: &Value) -> Value {
    let Some(id) = message.get("id").cloned() else {
        // A notification; the SDK acknowledges these with an empty result.
        return json!({"jsonrpc": "2.0", "result": {}, "id": 0});
    };
    let params = &message["params"];
    let result = match message.get("method").and_then(Value::as_str) {
        Some("initialize") => server_info(
            params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(MCP_PROTOCOL_VERSION),
        ),
        Some("tools/list") => json!({"tools": shared.definitions}),
        Some("ping") => json!({}),
        Some("tools/call") => {
            let tool_id = params
                .pointer("/_meta/claudecode~1toolUseId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| shared.tools.take_next());
            match tool_id {
                Some(tool_id) => shared
                    .tools
                    .wait(&tool_id)
                    .await
                    .unwrap_or_else(|_| cancelled_result()),
                None => json!({
                    "content": [{"type": "text", "text": "Forge could not match this tool call to its id."}],
                    "isError": true,
                }),
            }
        }
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": "Method not found"},
            })
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
