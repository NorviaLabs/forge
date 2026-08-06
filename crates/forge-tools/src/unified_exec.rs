use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::builtins::schema_for;
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecCommandArgs {
    pub cmd: String,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub login: bool,
    #[serde(default)]
    pub tty: bool,
    #[serde(default = "default_exec_yield")]
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteStdinArgs {
    pub session_id: u64,
    #[serde(default)]
    pub chars: String,
    #[serde(default = "default_stdin_yield")]
    pub yield_time_ms: u64,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
}

fn default_exec_yield() -> u64 {
    10_000
}
fn default_stdin_yield() -> u64 {
    250
}

struct Session {
    command: String,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    output: String,
    output_truncated: bool,
    started: Instant,
}

const MAX_SESSION_OUTPUT: usize = 1024 * 1024;

fn append_output(session: &mut Session, bytes: &[u8]) {
    if session.output.len() >= MAX_SESSION_OUTPUT {
        session.output_truncated = true;
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    let remaining = MAX_SESSION_OUTPUT - session.output.len();
    let end = text.len().min(remaining);
    let end = text.floor_char_boundary(end);
    session.output.push_str(&text[..end]);
    if end < text.len() {
        session.output_truncated = true;
    }
}

type Sessions = Arc<Mutex<HashMap<u64, Arc<Mutex<Session>>>>>;

fn sessions() -> &'static Sessions {
    static SESSIONS: OnceLock<Sessions> = OnceLock::new();
    SESSIONS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
}

fn output_for(session_id: u64, session: &Session, max_tokens: Option<usize>) -> Value {
    let raw = &session.output;
    let pre_truncation_tokens = raw.len().div_ceil(4);
    let limit = max_tokens.map(|tokens| tokens.saturating_mul(4));
    let output = match limit {
        Some(limit) if raw.len() > limit => {
            let end = raw.floor_char_boundary(limit);
            format!("{}\n[output truncated]", &raw[..end])
        }
        _ if session.output_truncated => format!("{raw}\n[output truncated]"),
        _ => raw.clone(),
    };
    json!({
        "session_id": session_id,
        "command": session.command,
        "running": session.child.id().is_some(),
        "output": output,
        "elapsed_ms": session.started.elapsed().as_millis(),
        "pre_truncation_tokens": pre_truncation_tokens,
    })
}

async fn collect(
    session_id: u64,
    session: &mut Session,
    wait: Duration,
    max_tokens: Option<usize>,
) -> Result<ToolOutput, ToolError> {
    let deadline = tokio::time::Instant::now() + wait;
    let mut stdout_buffer = [0_u8; 4096];
    let mut stderr_buffer = [0_u8; 4096];
    loop {
        if let Some(status) = session.child.try_wait().map_err(ToolError::Io)? {
            let exit = format!(
                "\n[process exited with code {}]",
                status.code().unwrap_or(-1)
            );
            append_output(session, exit.as_bytes());
            let mut tail = Vec::new();
            session.stdout.read_to_end(&mut tail).await?;
            append_output(session, &tail);
            let mut tail = Vec::new();
            session.stderr.read_to_end(&mut tail).await?;
            append_output(session, &tail);
            let body = output_for(session_id, session, max_tokens);
            return Ok(ToolOutput {
                outcome: Default::default(),
                content: body.to_string(),
                is_error: !status.success(),
                exit_code: status.code(),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::select! {
            result = session.stdout.read(&mut stdout_buffer) => {
                let count = result?;
                if count > 0 { append_output(session, &stdout_buffer[..count]); }
            }
            result = session.stderr.read(&mut stderr_buffer) => {
                let count = result?;
                if count > 0 { append_output(session, &stderr_buffer[..count]); }
            }
            _ = tokio::time::sleep(remaining.min(Duration::from_millis(20))) => {}
        }
    }
    let body = output_for(session_id, session, max_tokens);
    Ok(ToolOutput {
        outcome: Default::default(),
        content: body.to_string(),
        is_error: false,
        exit_code: None,
    })
}

async fn start(ctx: &ToolContext, args: ExecCommandArgs) -> Result<ToolOutput, ToolError> {
    if args.tty {
        return Err(ToolError::Execution(
            "exec_command tty mode is not supported yet".into(),
        ));
    }
    let mut command = if let Some(shell) = args.shell.as_deref() {
        let mut command = Command::new(shell);
        if args.login {
            command.arg("-l");
        }
        command.args(["-c", &args.cmd]);
        command
    } else {
        let mut command = Command::new("sh");
        if args.login {
            command.arg("-l");
        }
        command.args(["-c", &args.cmd]);
        command
    };
    let mut child = command
        .current_dir(&ctx.workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let session = Session {
        command: args.cmd.clone(),
        stdin: Some(
            child
                .stdin
                .take()
                .ok_or_else(|| ToolError::Execution("failed to open stdin".into()))?,
        ),
        stdout: child
            .stdout
            .take()
            .ok_or_else(|| ToolError::Execution("failed to open stdout".into()))?,
        stderr: child
            .stderr
            .take()
            .ok_or_else(|| ToolError::Execution("failed to open stderr".into()))?,
        child,
        output: String::new(),
        output_truncated: false,
        started: Instant::now(),
    };
    let id = next_id();
    let session = Arc::new(Mutex::new(session));
    sessions().lock().await.insert(id, session.clone());
    let mut session = session.lock().await;
    let result = collect(
        id,
        &mut session,
        Duration::from_millis(args.yield_time_ms),
        args.max_output_tokens,
    )
    .await;
    if result
        .as_ref()
        .is_ok_and(|output| output.exit_code.is_some())
    {
        sessions().lock().await.remove(&id);
    }
    result
}

fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct ExecCommandTool;
pub struct WriteStdinTool;

#[async_trait]
impl Tool for ExecCommandTool {
    fn name(&self) -> &str {
        "exec_command"
    }
    fn description(&self) -> &str {
        "Start a shell command and return partial output, retaining a session for polling or input"
    }
    fn input_schema(&self) -> Value {
        schema_for::<ExecCommandArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Exec
    }
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        start(
            ctx,
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?,
        )
        .await
    }
}

#[async_trait]
impl Tool for WriteStdinTool {
    fn name(&self) -> &str {
        "write_stdin"
    }
    fn description(&self) -> &str {
        "Send input to or poll an interactive shell session"
    }
    fn input_schema(&self) -> Value {
        schema_for::<WriteStdinArgs>()
    }
    fn side_effect_class(&self) -> SideEffectClass {
        SideEffectClass::Exec
    }
    async fn call(&self, _ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WriteStdinArgs =
            serde_json::from_value(args).map_err(|e| ToolError::Execution(e.to_string()))?;
        let session = sessions()
            .lock()
            .await
            .get(&args.session_id)
            .cloned()
            .ok_or_else(|| {
                ToolError::Execution(format!("unknown shell session {}", args.session_id))
            })?;
        let mut session = session.lock().await;
        if !args.chars.is_empty() {
            session
                .stdin
                .as_mut()
                .ok_or_else(|| ToolError::Execution("shell stdin is closed".into()))?
                .write_all(args.chars.as_bytes())
                .await?;
        }
        let result = collect(
            args.session_id,
            &mut session,
            Duration::from_millis(args.yield_time_ms),
            args.max_output_tokens,
        )
        .await;
        if result
            .as_ref()
            .is_ok_and(|output| output.exit_code.is_some())
        {
            sessions().lock().await.remove(&args.session_id);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn starts_running_session_and_polls_without_input() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let first = ExecCommandTool
            .call(
                &ctx,
                json!({"cmd": "printf ready; sleep 1", "yield_time_ms": 20}),
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&first.content).unwrap();
        assert!(body["session_id"].as_u64().is_some());
        assert!(body["output"].as_str().unwrap().contains("ready"));
        let id = body["session_id"].as_u64().unwrap();
        let polled = WriteStdinTool
            .call(&ctx, json!({"session_id": id, "yield_time_ms": 1200}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&polled.content).unwrap()["running"],
            false
        );
    }

    #[tokio::test]
    async fn empty_chars_does_not_write_to_stdin() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let first = ExecCommandTool
            .call(
                &ctx,
                json!({"cmd": "read x; echo got:$x", "yield_time_ms": 20}),
            )
            .await
            .unwrap();
        let id = serde_json::from_str::<Value>(&first.content).unwrap()["session_id"]
            .as_u64()
            .unwrap();
        let polled = WriteStdinTool
            .call(
                &ctx,
                json!({"session_id": id, "chars": "", "yield_time_ms": 20}),
            )
            .await
            .unwrap();
        let polled_body: Value = serde_json::from_str(&polled.content).unwrap();
        assert_eq!(polled_body["running"], true);

        let completed = WriteStdinTool
            .call(
                &ctx,
                json!({"session_id": id, "chars": "input\n", "yield_time_ms": 1000}),
            )
            .await
            .unwrap();
        assert!(completed.content.contains("got:input"));
    }
}
