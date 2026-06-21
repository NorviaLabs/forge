//! LiteLlmModelClient — sole production ModelClient (litellm-providers.md).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use forge_config::{Config, LitellmLifecycle, ModelProviderKind};
use forge_types::ModelResponse;
use tokio::time::timeout;

use crate::normalize::{complete_result_from_value, forge_messages_to_wire, tools_to_openai_functions};
use crate::wire::{CompleteParams, WireEnvelope, WireErrorBody, WireType};
use crate::{ModelClient, ModelError, ModelRequest};

static REQ_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    REQ_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct LiteLlmInner {
    python: String,
    module: String,
    worker_path: Option<String>,
    lifecycle: LitellmLifecycle,
    request_timeout: Duration,
    default_model: String,
    command_override: Option<Vec<String>>,
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
                worker: Mutex::new(None),
            }),
        }
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

        let mut child = cmd.spawn().map_err(|e| {
            ModelError::Worker(format!(
                "failed to spawn LiteLLM worker ({e}). Install Python + `pip install -e workers/forge-litellm-worker` or use --mock"
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
        Ok(WorkerProcess {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn write_env(w: &mut WorkerProcess, env: &WireEnvelope) -> Result<(), ModelError> {
        let line = env
            .encode_line()
            .map_err(|e| ModelError::Protocol(e.to_string()))?;
        w.stdin
            .write_all(line.as_bytes())
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        w.stdin
            .flush()
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        Ok(())
    }

    fn read_env(w: &mut WorkerProcess) -> Result<WireEnvelope, ModelError> {
        let mut line = String::new();
        let n = w
            .stdout
            .read_line(&mut line)
            .map_err(|e| ModelError::Transport(e.to_string()))?;
        if n == 0 {
            return Err(ModelError::Transport("worker closed stdout".into()));
        }
        WireEnvelope::decode_line(&line).map_err(|e| ModelError::Protocol(e.to_string()))
    }

    fn ensure_worker_locked(&self, slot: &mut Option<WorkerProcess>) -> Result<(), ModelError> {
        if slot.is_some() {
            return Ok(());
        }
        let mut w = self.spawn_worker()?;
        let ping = WireEnvelope::ping(next_id());
        Self::write_env(&mut w, &ping)?;
        let resp = Self::read_env(&mut w)?;
        if resp.is_error() {
            let msg = resp
                .error
                .map(|e| e.message)
                .unwrap_or_else(|| "ping failed".into());
            let _ = w.child.kill();
            return Err(ModelError::Worker(msg));
        }
        if resp.msg_type != WireType::Response {
            let _ = w.child.kill();
            return Err(ModelError::Protocol("ping expected response".into()));
        }
        *slot = Some(w);
        Ok(())
    }

    fn complete_blocking(&self, req: ModelRequest) -> Result<ModelResponse, ModelError> {
        let model = if req.model.is_empty() {
            self.default_model.clone()
        } else {
            req.model.clone()
        };
        let params = CompleteParams {
            model,
            messages: forge_messages_to_wire(&req.messages),
            tools: tools_to_openai_functions(&req.tools),
            temperature: None,
            max_tokens: None,
            extra: None,
        };
        let id = next_id();
        let request = WireEnvelope::complete(&id, &params)
            .map_err(|e| ModelError::Protocol(e.to_string()))?;

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
            let env = Self::read_env(w)?;
            if env.id != id {
                continue;
            }
            match env.msg_type {
                WireType::Event => continue,
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
                    return complete_result_from_value(&result);
                }
                WireType::Request => {
                    return Err(ModelError::Protocol("unexpected request from worker".into()));
                }
            }
        }
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
                }],
                tools: vec![],
                model: "openai/gpt-test".into(),
            })
            .await
            .unwrap();
        assert_eq!(resp.text, "hello from fixture");
        assert_eq!(resp.usage.unwrap().completion_tokens, 2);
    }
}
