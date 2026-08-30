use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
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

fn sandbox_denial(
    confined: bool,
    success: bool,
    content: &str,
    stderr: &str,
    shell: &str,
    workspace_root: &std::path::Path,
    denied_host: Option<String>,
) -> Option<ToolError> {
    if !confined {
        return None;
    }
    crate::egress::denial_for_confined_command(
        content,
        stderr,
        success,
        shell,
        workspace_root,
        denied_host,
    )
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
    shell: String,
    confined: bool,
    workspace_root: PathBuf,
    egress_invocation: Option<crate::egress::EgressInvocation>,
    _session_tmp: Option<Arc<crate::SessionTempDir>>,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    output: String,
    stderr_output: String,
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

fn append_stderr(session: &mut Session, bytes: &[u8]) {
    append_output(session, bytes);
    if session.stderr_output.len() >= MAX_SESSION_OUTPUT {
        return;
    }
    let text = String::from_utf8_lossy(bytes);
    let remaining = MAX_SESSION_OUTPUT - session.stderr_output.len();
    let end = text.len().min(remaining);
    let end = text.floor_char_boundary(end);
    session.stderr_output.push_str(&text[..end]);
}

fn session_finished(result: &Result<ToolOutput, ToolError>) -> bool {
    result
        .as_ref()
        .is_ok_and(|output| output.exit_code.is_some())
        || matches!(result, Err(ToolError::SandboxDenied { .. }))
}

/// Sessions belong to one tool-registry installation. They must never be
/// process-global: independent agent sessions and subagents may share a
/// process, but must not be able to poll or write each other's shells.
#[derive(Default)]
struct ExecSessionStore {
    next_id: AtomicU64,
    sessions: Mutex<HashMap<u64, Arc<Mutex<Session>>>>,
}

impl ExecSessionStore {
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }
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
            append_stderr(session, &tail);
            let denied_host = session
                .egress_invocation
                .as_ref()
                .and_then(crate::egress::EgressInvocation::take_denied_host);
            if let Some(error) = sandbox_denial(
                session.confined,
                status.success(),
                &session.output,
                &session.stderr_output,
                &session.shell,
                &session.workspace_root,
                denied_host,
            ) {
                return Err(error);
            }
            let body = output_for(session_id, session, max_tokens);
            return Ok(ToolOutput {
                outcome: Default::default(),
                content: body.to_string(),
                is_error: !status.success(),
                exit_code: status.code(),
                attachments: Vec::new(),
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
                if count > 0 { append_stderr(session, &stderr_buffer[..count]); }
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
        attachments: Vec::new(),
    })
}

async fn start(
    sessions: &ExecSessionStore,
    ctx: &ToolContext,
    args: ExecCommandArgs,
) -> Result<ToolOutput, ToolError> {
    if args.tty {
        return Err(ToolError::Execution(
            "exec_command tty mode is not supported yet".into(),
        ));
    }
    // Login shells source profile files, which can re-export provider
    // credentials after the explicit removals below.
    if args.login {
        return Err(ToolError::Execution(
            "exec_command login shells are not supported".into(),
        ));
    }
    // These sessions outlive the turn that started them and `write_stdin`
    // feeds them afterwards, so confinement has to be applied here at spawn.
    // There is no way to sandbox the session later, and `write_stdin` is not
    // itself gated — a session that starts unconfined accepts arbitrary
    // commands unconfined for as long as it lives.
    let shell = args.shell.as_deref().unwrap_or("sh");
    let requested_confined = !ctx.unconfined_shell;
    let egress_invocation = if requested_confined {
        crate::egress::EgressInvocation::start(ctx.egress.as_deref())
            .await
            .map_err(|error| {
                ToolError::Execution(format!("failed to start invocation egress proxy: {error}"))
            })?
    } else {
        None
    };
    let command_egress = egress_invocation
        .as_ref()
        .map(crate::egress::EgressInvocation::grant)
        .or(ctx.egress.as_deref());
    let mut policy = crate::sandbox::SandboxPolicy::for_workspace(&ctx.workspace_root)
        .with_command_access(&args.cmd)
        .with_egress(command_egress);
    if crate::credentials::needs_git_writes(&args.cmd) {
        policy = policy.with_git_writable();
    }
    if let Some(session_tmp) = &ctx.session_tmp {
        policy = policy.with_session_tmp(session_tmp.path());
    }
    let wrapped = requested_confined
        .then(|| crate::sandbox::wrap_shell_command(shell, &args.cmd, &policy))
        .flatten();
    let confined = wrapped.is_some();
    let mut command = match wrapped {
        Some((program, wrapped)) => {
            let mut confined = Command::new(program);
            confined.args(wrapped);
            confined
        }
        None => {
            let mut plain = Command::new(shell);
            plain.args(["-c", &args.cmd]);
            plain
        }
    };
    for name in crate::builtins::PROVIDER_CREDENTIAL_ENV {
        command.env_remove(name);
    }
    for (name, value) in crate::sandbox::temp_env(&policy) {
        command.env(name, value);
    }
    if confined {
        for name in crate::credentials::HOST_CREDENTIAL_ENV {
            command.env_remove(name);
        }
        for (name, value) in crate::sandbox::egress_env(&policy) {
            command.env(name, value);
        }
        let identity_dir = ctx
            .session_tmp
            .as_ref()
            .map(|dir| dir.path().join("host-identity"))
            .unwrap_or_else(|| ctx.workspace_root.join(".forge-host-identity"));
        let _ = std::fs::create_dir_all(&identity_dir);
        for (name, value) in crate::credentials::isolated_config_env(&identity_dir) {
            command.env(name, value);
        }
        for (name, value) in crate::credentials::host_identity_env(command_egress, &identity_dir) {
            command.env(name, value);
        }
        for (name, value) in policy.toolchain_env() {
            command.env(name, value);
        }
    }
    let mut child = command
        .current_dir(&ctx.workspace_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let session = Session {
        command: args.cmd.clone(),
        shell: shell.to_string(),
        confined,
        workspace_root: ctx.workspace_root.clone(),
        egress_invocation,
        _session_tmp: ctx.session_tmp.clone(),
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
        stderr_output: String::new(),
        output_truncated: false,
        started: Instant::now(),
    };
    let id = sessions.next_id();
    let session = Arc::new(Mutex::new(session));
    sessions.sessions.lock().await.insert(id, session.clone());
    let mut session = session.lock().await;
    let result = collect(
        id,
        &mut session,
        Duration::from_millis(args.yield_time_ms),
        args.max_output_tokens,
    )
    .await;
    if session_finished(&result) {
        sessions.sessions.lock().await.remove(&id);
    }
    result
}

pub struct ExecCommandTool {
    sessions: Arc<ExecSessionStore>,
}

pub struct WriteStdinTool {
    sessions: Arc<ExecSessionStore>,
}

/// Creates the paired tools sharing one registry-scoped shell-session store.
pub fn unified_exec_tools() -> (ExecCommandTool, WriteStdinTool) {
    let sessions = Arc::new(ExecSessionStore::default());
    (
        ExecCommandTool {
            sessions: Arc::clone(&sessions),
        },
        WriteStdinTool { sessions },
    )
}

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
            &self.sessions,
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
        let session = self
            .sessions
            .sessions
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
        if session_finished(&result) {
            self.sessions.sessions.lock().await.remove(&args.session_id);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn failed_confined_network_command_is_a_sandbox_denial() {
        let dir = tempdir().unwrap();
        let output = "curl: (6) Could not resolve host: api.github.com";
        let error = sandbox_denial(true, false, output, "", "sh", dir.path(), None)
            .expect("a confined network failure must be escalated");

        assert!(matches!(error, ToolError::SandboxDenied { .. }));
    }

    #[test]
    fn unconfined_network_failure_is_not_a_sandbox_denial() {
        let dir = tempdir().unwrap();
        let output = "curl: (6) Could not resolve host: api.github.com";

        assert!(sandbox_denial(false, false, output, "", "sh", dir.path(), None).is_none());
    }

    #[test]
    fn successful_stderr_can_still_report_a_sandbox_denial() {
        let dir = tempdir().unwrap();
        let output = "sh: /outside/file: Operation not permitted";

        assert!(matches!(
            sandbox_denial(true, true, output, output, "sh", dir.path(), None),
            Some(ToolError::SandboxDenied { .. })
        ));
    }

    #[test]
    fn successful_stdout_does_not_invent_a_sandbox_denial() {
        let dir = tempdir().unwrap();
        let output = "Operation not permitted";

        assert!(sandbox_denial(true, true, output, "", "sh", dir.path(), None).is_none());
    }

    #[test]
    fn successful_child_warning_does_not_invent_a_sandbox_denial() {
        let dir = tempdir().unwrap();
        let output = "git: error: couldn't create cache file '/tmp/x': Operation not permitted";

        assert!(sandbox_denial(true, true, output, output, "sh", dir.path(), None).is_none());
    }

    #[tokio::test]
    async fn exec_command_pipeline_cannot_hide_a_sandbox_denial() {
        if crate::sandbox::availability().is_err() {
            return;
        }
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = outside.path().join("pipeline-escape.txt");
        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let (exec_command, _) = unified_exec_tools();

        let error = exec_command
            .call(
                &ctx,
                json!({
                    "cmd": format!("printf escaped > {} | cat", target.display()),
                    "yield_time_ms": 1_000
                }),
            )
            .await
            .expect_err("the pipeline's zero status must not hide the denied write");

        assert!(matches!(error, ToolError::SandboxDenied { .. }));
        assert!(!target.exists(), "exec_command escaped the sandbox");
    }

    #[tokio::test]
    async fn approved_exec_command_does_not_keep_the_sandbox_proxy() {
        let dir = tempdir().unwrap();
        let mut ctx = ToolContext::new(dir.path().to_path_buf()).with_unconfined_shell();
        ctx.egress = Some(std::sync::Arc::new(crate::sandbox::EgressGrant {
            proxy_port: 9418,
            socket_path: dir.path().join("egress.sock"),
            control: None,
        }));
        let (exec_command, _) = unified_exec_tools();
        let first = exec_command
            .call(
                &ctx,
                json!({
                    "cmd": "printf '%s|%s' \"$HTTP_PROXY\" \"$HTTPS_PROXY\"",
                    "yield_time_ms": 1_000
                }),
            )
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&first.content).unwrap();
        let printed = body["output"].as_str().unwrap_or_default();
        assert!(
            !printed.contains("127.0.0.1:9418") && !printed.contains("127.0.0.1:8118"),
            "sandbox proxy leaked into the unconfined exec session: {printed}"
        );
    }

    #[tokio::test]
    async fn starts_running_session_and_polls_without_input() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let (exec_command, write_stdin) = unified_exec_tools();
        let first = exec_command
            .call(
                &ctx,
                json!({"cmd": "printf ready; sleep 1", "yield_time_ms": 20}),
            )
            .await
            .unwrap();
        let mut body: Value = serde_json::from_str(&first.content).unwrap();
        let id = body["session_id"]
            .as_u64()
            .expect("exec_command should retain a session id");
        // A 20ms first yield can miss stdout on a loaded runner. Keep the
        // session running and poll until the initial output arrives.
        for _ in 0..50 {
            if body["output"]
                .as_str()
                .unwrap_or_default()
                .contains("ready")
            {
                break;
            }
            assert_eq!(
                body["running"], true,
                "session exited before printing ready: {body}"
            );
            let polled = write_stdin
                .call(&ctx, json!({"session_id": id, "yield_time_ms": 50}))
                .await
                .unwrap();
            body = serde_json::from_str(&polled.content).unwrap();
        }
        assert!(
            body["output"].as_str().unwrap().contains("ready"),
            "expected ready in session output, got {body}"
        );
        let finished = write_stdin
            .call(&ctx, json!({"session_id": id, "yield_time_ms": 1200}))
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&finished.content).unwrap()["running"],
            false
        );
    }

    #[tokio::test]
    async fn sessions_cannot_cross_tool_registry_boundaries() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let (exec_command, _) = unified_exec_tools();
        let (_, other_write_stdin) = unified_exec_tools();
        let first = exec_command
            .call(&ctx, json!({"cmd": "sleep 1", "yield_time_ms": 20}))
            .await
            .unwrap();
        let id = serde_json::from_str::<Value>(&first.content).unwrap()["session_id"]
            .as_u64()
            .unwrap();

        let error = other_write_stdin
            .call(&ctx, json!({"session_id": id, "yield_time_ms": 20}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown shell session"));
    }

    #[tokio::test]
    async fn empty_chars_does_not_write_to_stdin() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let (exec_command, write_stdin) = unified_exec_tools();
        let first = exec_command
            .call(
                &ctx,
                json!({"cmd": "read x; echo got:$x", "yield_time_ms": 20}),
            )
            .await
            .unwrap();
        let id = serde_json::from_str::<Value>(&first.content).unwrap()["session_id"]
            .as_u64()
            .unwrap();
        let polled = write_stdin
            .call(
                &ctx,
                json!({"session_id": id, "chars": "", "yield_time_ms": 20}),
            )
            .await
            .unwrap();
        let polled_body: Value = serde_json::from_str(&polled.content).unwrap();
        assert_eq!(polled_body["running"], true);

        let completed = write_stdin
            .call(
                &ctx,
                json!({"session_id": id, "chars": "input\n", "yield_time_ms": 1000}),
            )
            .await
            .unwrap();
        assert!(completed.content.contains("got:input"));
    }

    #[tokio::test]
    async fn refuses_login_shells() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let (exec_command, _) = unified_exec_tools();
        let error = exec_command
            .call(&ctx, json!({"cmd": "true", "login": true}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("login"), "{error}");
    }

    #[tokio::test]
    async fn does_not_inherit_provider_credentials() {
        const VAR: &str = "OPENCODE_ZEN_API_KEY";
        let previous = std::env::var(VAR).ok();
        std::env::set_var(VAR, "sk-must-not-reach-the-child");
        struct Guard(Option<String>);
        impl Drop for Guard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("OPENCODE_ZEN_API_KEY", value),
                    None => std::env::remove_var("OPENCODE_ZEN_API_KEY"),
                }
            }
        }
        let _guard = Guard(previous);

        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let (exec_command, _) = unified_exec_tools();
        let out = exec_command
            .call(
                &ctx,
                json!({"cmd": "printf '[%s]' \"$OPENCODE_ZEN_API_KEY\"", "yield_time_ms": 1000}),
            )
            .await
            .unwrap();
        assert!(
            out.content.contains("[]"),
            "credential reached the child: {}",
            out.content
        );
    }
}
