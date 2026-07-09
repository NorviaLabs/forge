//! LiteLlmModelClient — sole production ModelClient (litellm-providers.md).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use async_trait::async_trait;
use forge_config::{Config, LitellmLifecycle, ModelProviderKind};
use forge_types::ModelResponse;
use tokio::time::timeout;

use crate::normalize::{complete_result_from_value, forge_messages_to_wire, tools_to_openai_functions};
use crate::wire::{CompleteParams, WireEnvelope, WireErrorBody, WireType};
use crate::{ModelClient, ModelError, ModelRequest, StreamEventTx};
use forge_types::ModelStreamEvent;

static REQ_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    REQ_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    _stderr_thread: Option<thread::JoinHandle<()>>,
}

impl WorkerProcess {
    fn take_stderr_snippet(&self) -> String {
        self.stderr_buf
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    fn dead_worker_message(&mut self, context: &str) -> String {
        let status = self.child.try_wait().ok().flatten();
        // Give stderr thread a moment to finish if the process just exited.
        thread::sleep(Duration::from_millis(50));
        let stderr = self.take_stderr_snippet();
        let stderr_trim = stderr.trim();
        let exit = status
            .map(|s| format!("{s}"))
            .unwrap_or_else(|| "unknown".into());
        let mut msg = format!("worker closed stdout while {context} (exit {exit})");
        if !stderr_trim.is_empty() {
            let snippet: String = stderr_trim.chars().take(500).collect();
            msg.push_str(": ");
            msg.push_str(&snippet.replace('\n', " | "));
        }
        if stderr_trim.contains("No module named 'forge_litellm_worker'")
            || stderr_trim.contains("No module named forge_litellm_worker")
        {
            msg.push_str(
                " — install the worker: `cd workers/forge-litellm-worker && pip install -e .` \
(or `pip install litellm` + set PYTHONPATH to workers/forge-litellm-worker/src)",
            );
        } else if stderr_trim.contains("No module named 'litellm'") {
            msg.push_str(" — install LiteLLM: `pip install litellm`");
        } else if stderr_trim.is_empty() {
            msg.push_str(
                " — LiteLLM worker exited with no stderr. Ensure `python3 -m forge_litellm_worker` works \
(pip install -e workers/forge-litellm-worker).",
            );
        }
        msg
    }
}

struct LiteLlmInner {
    python: String,
    module: String,
    worker_path: Option<String>,
    lifecycle: LitellmLifecycle,
    request_timeout: Duration,
    default_model: String,
    command_override: Option<Vec<String>>,
    /// Extra env injected at spawn (connect OAuth → XAI_API_KEY, etc.).
    extra_env: Mutex<BTreeMap<String, String>>,
    worker: Mutex<Option<WorkerProcess>>,
}

pub struct LiteLlmModelClient {
    inner: Arc<LiteLlmInner>,
}

impl LiteLlmModelClient {
    pub fn from_config(cfg: &Config) -> Result<Self, ModelError> {
        if cfg.model.provider != ModelProviderKind::Litellm {
            return Err(ModelError::Other(
                "LiteLlmModelClient requires provider=litellm".into(),
            ));
        }
        if cfg.model.model.trim().is_empty() {
            return Err(ModelError::Other(
                "model id required for litellm provider".into(),
            ));
        }
        Ok(Self {
            inner: Arc::new(LiteLlmInner {
                python: cfg.model.litellm.python.clone(),
                module: cfg.model.litellm.module.clone(),
                worker_path: cfg.model.litellm.worker_path.clone(),
                lifecycle: cfg.model.litellm.lifecycle.clone(),
                request_timeout: Duration::from_secs(cfg.model.litellm.request_timeout_secs.max(1)),
                default_model: cfg.model.model.clone(),
                command_override: None,
                extra_env: Mutex::new(BTreeMap::new()),
                worker: Mutex::new(None),
            }),
        })
    }

    /// Test helper: run an arbitrary argv as the worker (first element is program).
    pub fn with_command(argv: Vec<String>, default_model: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(LiteLlmInner {
                python: String::new(),
                module: String::new(),
                worker_path: None,
                lifecycle: LitellmLifecycle::LongLived,
                request_timeout: Duration::from_secs(30),
                default_model: default_model.into(),
                command_override: Some(argv),
                extra_env: Mutex::new(BTreeMap::new()),
                worker: Mutex::new(None),
            }),
        }
    }

    /// Inject provider credentials (e.g. OAuth access token as XAI_API_KEY) and recycle worker.
    pub fn set_provider_env(&self, pairs: impl IntoIterator<Item = (String, String)>) {
        if let Ok(mut g) = self.inner.extra_env.lock() {
            for (k, v) in pairs {
                g.insert(k, v);
            }
        }
        self.recycle_worker();
    }

    pub fn recycle_worker(&self) {
        if let Ok(mut g) = self.inner.worker.lock() {
            if let Some(mut w) = g.take() {
                let _ = LiteLlmInner::write_env(&mut w, &WireEnvelope::shutdown(next_id()));
                let _ = w.child.kill();
                let _ = w.child.wait();
            }
        }
    }
}

/// Locate `workers/forge-litellm-worker/src` so `python -m forge_litellm_worker` works without
/// a global pip install (litellm itself must still be installed in that Python).
fn discover_worker_src() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("FORGE_LITELLM_WORKER_SRC") {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return Some(pb);
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.clone());
        // Walk up a few levels (monorepo / nested cwd)
        let mut cur = cwd;
        for _ in 0..5 {
            if let Some(parent) = cur.parent() {
                candidates.push(parent.to_path_buf());
                cur = parent.to_path_buf();
            } else {
                break;
            }
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.to_path_buf());
            if let Some(p) = dir.parent() {
                candidates.push(p.to_path_buf());
            }
        }
    }
    for root in candidates {
        let src = root.join("workers/forge-litellm-worker/src");
        if src.join("forge_litellm_worker").is_dir() {
            return Some(src);
        }
    }
    None
}

fn merge_pythonpath(extra: &Path) -> String {
    let extra_s = extra.display().to_string();
    match std::env::var("PYTHONPATH") {
        Ok(existing) if !existing.is_empty() => {
            if existing.split(':').any(|p| p == extra_s) {
                existing
            } else {
                format!("{extra_s}:{existing}")
            }
        }
        _ => extra_s,
    }
}

impl LiteLlmInner {
    fn spawn_worker(&self) -> Result<WorkerProcess, ModelError> {
        let mut cmd = if let Some(ref argv) = self.command_override {
            let mut c = Command::new(&argv[0]);
            if argv.len() > 1 {
                c.args(&argv[1..]);
            }
            c
        } else if let Some(ref path) = self.worker_path {
            let mut c = Command::new(&self.python);
            c.arg(path);
            c
        } else {
            let mut c = Command::new(&self.python);
            c.arg("-m").arg(&self.module);
            c
        };
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Unbuffered Python so NDJSON flushes promptly
        cmd.env("PYTHONUNBUFFERED", "1");

        if let Some(src) = discover_worker_src() {
            cmd.env("PYTHONPATH", merge_pythonpath(&src));
        }

        if let Ok(extra) = self.extra_env.lock() {
            for (k, v) in extra.iter() {
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            ModelError::Worker(format!(
                "failed to spawn LiteLLM worker ({e}). Install Python + `pip install -e workers/forge-litellm-worker` \
(or set model.litellm.python / FORGE_LITELLM_PYTHON)."
            ))
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ModelError::Worker("worker stdin missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ModelError::Worker("worker stdout missing".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| ModelError::Worker("worker stderr missing".into()))?;

        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let buf_clone = stderr_buf.clone();
        let stderr_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut chunk = String::new();
            loop {
                chunk.clear();
                match reader.read_line(&mut chunk) {
                    Ok(0) => break,
                    Ok(_) => {
                        if let Ok(mut g) = buf_clone.lock() {
                            // Cap stderr buffer so a noisy worker cannot grow unbounded.
                            if g.len() < 8_192 {
                                g.push_str(&chunk);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            // Drain any remaining bytes
            let mut rest = Vec::new();
            let _ = reader.read_to_end(&mut rest);
            if !rest.is_empty() {
                if let Ok(mut g) = buf_clone.lock() {
                    if g.len() < 8_192 {
                        g.push_str(&String::from_utf8_lossy(&rest));
                    }
                }
            }
        });

        Ok(WorkerProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr_buf,
            _stderr_thread: Some(stderr_thread),
        })
    }

    fn write_env(w: &mut WorkerProcess, env: &WireEnvelope) -> Result<(), ModelError> {
        let line = env
            .encode_line()
            .map_err(|e| ModelError::Protocol(e.to_string()))?;
        if let Err(e) = w.stdin.write_all(line.as_bytes()).and_then(|_| w.stdin.flush()) {
            return Err(ModelError::Transport(w.dead_worker_message(&format!(
                "sending message ({e})"
            ))));
        }
        Ok(())
    }

    fn read_env(w: &mut WorkerProcess) -> Result<WireEnvelope, ModelError> {
        // LiteLLM (and friends) sometimes leak blank lines / banners to stdout.
        // Skip non-JSON noise until we get a wire frame or EOF.
        const MAX_SKIP: usize = 64;
        let mut skipped = 0usize;
        loop {
            let mut line = String::new();
            let n = w
                .stdout
                .read_line(&mut line)
                .map_err(|e| ModelError::Transport(e.to_string()))?;
            if n == 0 {
                return Err(ModelError::Transport(
                    w.dead_worker_message("reading response"),
                ));
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                skipped += 1;
                if skipped > MAX_SKIP {
                    return Err(ModelError::Protocol(
                        "worker produced only blank lines (stdout polluted?)".into(),
                    ));
                }
                continue;
            }
            // ANSI / human banners from litellm are not JSON
            if !trimmed.starts_with('{') {
                skipped += 1;
                if skipped > MAX_SKIP {
                    return Err(ModelError::Protocol(format!(
                        "worker stdout not NDJSON after {MAX_SKIP} lines (sample: {})",
                        trimmed.chars().take(80).collect::<String>()
                    )));
                }
                continue;
            }
            return WireEnvelope::decode_line(trimmed)
                .map_err(|e| ModelError::Protocol(e.to_string()));
        }
    }

    fn ensure_worker_locked(&self, slot: &mut Option<WorkerProcess>) -> Result<(), ModelError> {
        if slot.is_some() {
            return Ok(());
        }
        let mut w = self.spawn_worker()?;
        let ping = WireEnvelope::ping(next_id());
        Self::write_env(&mut w, &ping)?;
        let resp = match Self::read_env(&mut w) {
            Ok(r) => r,
            Err(e) => {
                let _ = w.child.kill();
                return Err(e);
            }
        };
        if resp.is_error() {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "ping failed".into());
            let stderr = w.take_stderr_snippet();
            let _ = w.child.kill();
            let mut full = msg;
            if !stderr.trim().is_empty() {
                full.push_str(" | stderr: ");
                full.push_str(stderr.trim());
            }
            return Err(ModelError::Worker(full));
        }
        if resp.msg_type != WireType::Response {
            let _ = w.child.kill();
            return Err(ModelError::Protocol("ping expected response".into()));
        }
        *slot = Some(w);
        Ok(())
    }

    fn build_params(&self, req: &ModelRequest) -> CompleteParams {
        let model = if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        };
        CompleteParams {
            model,
            messages: forge_messages_to_wire(&req.messages),
            tools: tools_to_openai_functions(&req.tools),
            temperature: None,
            max_tokens: None,
            extra: None,
        }
    }

    fn complete_blocking(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        let params = self.build_params(&req);
        let id = next_id();
        let request = WireEnvelope::complete(&id, &params)
            .map_err(|e| ModelError::Protocol(e.to_string()))?;
        self.roundtrip_complete(&id, request, None)
    }

    /// Streaming complete when no tools; with tools fall back to non-stream
    /// (tool-call streaming is not reliable across all providers).
    fn complete_stream_blocking(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        if !req.tools.is_empty() {
            let resp = self.complete_blocking(req)?;
            if let Some(ref tx) = tx {
                if let Some(ref thinking) = resp.thinking {
                    if !thinking.is_empty() {
                        let _ = tx.send(ModelStreamEvent::ThinkingDelta {
                            text: thinking.clone(),
                        });
                    }
                }
                if !resp.text.is_empty() {
                    let _ = tx.send(ModelStreamEvent::TextDelta {
                        text: resp.text.clone(),
                    });
                }
                let _ = tx.send(ModelStreamEvent::MessageEnd);
            }
            return Ok(resp);
        }

        let params = self.build_params(&req);
        let id = next_id();
        let request = WireEnvelope::complete_stream(&id, &params)
            .map_err(|e| ModelError::Protocol(e.to_string()))?;
        self.roundtrip_complete(&id, request, tx)
    }

    fn roundtrip_complete(
        &self,
        id: &str,
        request: WireEnvelope,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let mut guard = self
            .worker
            .lock()
            .map_err(|_| ModelError::Other("worker lock poisoned".into()))?;

        if matches!(self.lifecycle, LitellmLifecycle::PerCall) {
            if let Some(mut old) = guard.take() {
                let _ = old.child.kill();
            }
        }

        self.ensure_worker_locked(&mut guard)?;
        let w = guard.as_mut().unwrap();
        Self::write_env(w, &request)?;

        loop {
            let env = match Self::read_env(w) {
                Ok(e) => e,
                Err(e) => {
                    let _ = guard.take();
                    return Err(e);
                }
            };
            if env.id != id {
                continue;
            }
            match env.msg_type {
                WireType::Event => {
                    if let Some(ref tx) = tx {
                        if let Some(ev) = parse_stream_event(env.params.as_ref()) {
                            let _ = tx.send(ev);
                        }
                    }
                }
                WireType::Error => {
                    let err = env.error.unwrap_or(WireErrorBody {
                        code: "internal".into(),
                        message: "unknown worker error".into(),
                        data: None,
                    });
                    return Err(map_wire_error(&err.code, &err.message));
                }
                WireType::Response => {
                    if let Some(err) = env.error {
                        return Err(map_wire_error(&err.code, &err.message));
                    }
                    let result = env.result.ok_or_else(|| {
                        ModelError::Protocol("complete response missing result".into())
                    })?;
                    // complete_stream result is forge-shaped (text/tool_calls/usage)
                    let resp = complete_result_from_value(&result)?;
                    if let Some(ref tx) = tx {
                        let _ = tx.send(ModelStreamEvent::MessageEnd);
                    }
                    return Ok(resp);
                }
                WireType::Request => {
                    return Err(ModelError::Protocol("unexpected request from worker".into()));
                }
            }
        }
    }
}

fn parse_stream_event(params: Option<&serde_json::Value>) -> Option<ModelStreamEvent> {
    let p = params?;
    let kind = p.get("kind")?.as_str()?;
    match kind {
        "text_delta" => Some(ModelStreamEvent::TextDelta {
            text: p.get("text")?.as_str()?.to_string(),
        }),
        "thinking_delta" => Some(ModelStreamEvent::ThinkingDelta {
            text: p.get("text")?.as_str()?.to_string(),
        }),
        "usage" => {
            let usage = forge_types::Usage {
                prompt_tokens: p.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0)
                    as u32,
                completion_tokens: p
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32,
            };
            Some(ModelStreamEvent::Usage { usage })
        }
        _ => None,
    }
}

fn map_wire_error(code: &str, message: &str) -> ModelError {
    match code {
        "upstream_auth" => ModelError::MissingApiKey,
        "upstream_rate_limit" | "upstream" => ModelError::Provider(message.into()),
        "protocol" | "invalid_params" => ModelError::Protocol(message.into()),
        "internal" => ModelError::Worker(message.into()),
        _ => ModelError::Provider(format!("{code}: {message}")),
    }
}

impl Drop for LiteLlmInner {
    fn drop(&mut self) {
        if let Ok(mut g) = self.worker.lock() {
            if let Some(mut w) = g.take() {
                let _ = Self::write_env(&mut w, &WireEnvelope::shutdown(next_id()));
                let _ = w.child.kill();
                let _ = w.child.wait();
            }
        }
    }
}

#[async_trait]
impl ModelClient for LiteLlmModelClient {
    async fn complete(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        let inner = self.inner.clone();
        let this_timeout = inner.request_timeout;
        let result = timeout(
            this_timeout,
            tokio::task::spawn_blocking(move || inner.complete_blocking(req)),
        )
        .await;

        match result {
            Ok(Ok(inner)) => inner,
            Ok(Err(e)) => Err(ModelError::Other(format!("join error: {e}"))),
            Err(_) => Err(ModelError::Transport("request timed out".into())),
        }
    }

    async fn complete_with_stream(
        &self,
        req: ModelRequest,
        tx: Option<StreamEventTx>,
    ) -> Result<ModelResponse, ModelError> {
        let inner = self.inner.clone();
        let this_timeout = inner.request_timeout;
        let result = timeout(
            this_timeout,
            tokio::task::spawn_blocking(move || inner.complete_stream_blocking(req, tx)),
        )
        .await;

        match result {
            Ok(Ok(inner)) => inner,
            Ok(Err(e)) => Err(ModelError::Other(format!("join error: {e}"))),
            Err(_) => Err(ModelError::Transport("request timed out".into())),
        }
    }

    fn apply_provider_env(&self, pairs: &[(String, String)]) {
        self.set_provider_env(pairs.iter().cloned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::{Message, MessageRole};
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn fixture_worker_script() -> NamedTempFile {
        let mut f = NamedTempFile::with_suffix(".py").unwrap();
        // Minimal NDJSON worker: ping + complete with canned text.
        writeln!(
            f,
            r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    mid = msg.get("id", "0")
    method = msg.get("method")
    if method == "ping":
        print(json.dumps({{"v":1,"id":mid,"type":"response","method":"ping","result":{{"ok":True,"python_version":"test","litellm_version":"fixture"}}}}), flush=True)
    elif method == "shutdown":
        print(json.dumps({{"v":1,"id":mid,"type":"response","method":"shutdown","result":{{"ok":True}}}}), flush=True)
        break
    elif method == "complete":
        print(json.dumps({{"v":1,"id":mid,"type":"response","method":"complete","result":{{"text":"hello from fixture","tool_calls":[],"usage":{{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}}}}}), flush=True)
    else:
        print(json.dumps({{"v":1,"id":mid,"type":"error","error":{{"code":"protocol","message":"unknown"}}}}), flush=True)
"#
        )
        .unwrap();
        f
    }

    #[tokio::test]
    async fn fixture_worker_complete() {
        let script = fixture_worker_script();
        let client = LiteLlmModelClient::with_command(
            vec!["python3".into(), script.path().display().to_string()],
            "openai/gpt-test",
        );
        let resp = client
            .complete(ModelRequest {
                messages: vec![Message {
                    role: MessageRole::User,
                    content: "hi".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
            }],
                tools: vec![],
                model: "openai/gpt-test".into(),
            })
            .await
            .unwrap();
        assert_eq!(resp.text, "hello from fixture");
        assert_eq!(resp.usage.unwrap().completion_tokens, 2);
    }

    #[tokio::test]
    async fn missing_worker_module_surfaces_stderr() {
        let client = LiteLlmModelClient::with_command(
            vec![
                "python3".into(),
                "-c".into(),
                "import sys; sys.stderr.write(\"No module named 'forge_litellm_worker'\\n\"); sys.exit(1)".into(),
            ],
            "xai/grok-3",
        );
        let err = client
            .complete(ModelRequest {
                messages: vec![Message {
                    role: MessageRole::User,
                    content: "hi".into(),
                    tool_call_id: None,
                    name: None,
                    thinking: None,
            }],
                tools: vec![],
                model: "xai/grok-3".into(),
            })
            .await
            .unwrap_err();
        let s = err.to_string();
        assert!(
            s.contains("worker closed stdout") || s.contains("forge_litellm_worker"),
            "unexpected error: {s}"
        );
        assert!(
            s.contains("pip install") || s.contains("forge_litellm_worker"),
            "should hint install: {s}"
        );
    }

    #[test]
    fn discover_worker_src_from_repo() {
        // When tests run from workspace, discovery should find the package.
        if Path::new("workers/forge-litellm-worker/src/forge_litellm_worker").is_dir() {
            assert!(discover_worker_src().is_some());
        }
    }
}
