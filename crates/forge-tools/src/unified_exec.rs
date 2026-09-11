use async_trait::async_trait;
use forge_types::{SideEffectClass, ToolOutput};
use portable_pty::{native_pty_system, Child as PtyChild, CommandBuilder, MasterPty, PtySize};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex as StdMutex};
use std::thread;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::builtins::{schema_for, set_process_group, ProcessGroupGuard};
use crate::registry::ToolContext;
use crate::{Tool, ToolError};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecCommandArgs {
    pub cmd: String,
    #[serde(default)]
    pub shell: Option<String>,
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
    owner: Option<forge_types::SessionId>,
    command: String,
    shell: String,
    confined: bool,
    workspace_root: PathBuf,
    egress_invocation: Option<crate::egress::EgressInvocation>,
    _session_tmp: Option<Arc<crate::SessionTempDir>>,
    process: Process,
    /// Owns the retained session's process group so `terminate_process`,
    /// `shutdown`, and dropping the session all reap the whole tree.
    process_group: ProcessGroupGuard,
    output: String,
    stderr_output: String,
    output_truncated: bool,
    started: Instant,
    running: bool,
}

enum Process {
    Pipe {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: ChildStdout,
        stderr: ChildStderr,
    },
    Pty(PtyProcess),
}

struct PtyProcess {
    // Kept for future caller-provided resize support. The fixed default size
    // is part of the current exec_command contract.
    _master: Box<dyn MasterPty + Send>,
    writer: Arc<StdMutex<Box<dyn Write + Send>>>,
    child: Box<dyn PtyChild + Send + Sync>,
    output_rx: mpsc::Receiver<Vec<u8>>,
    reader_done: Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

const PTY_DEFAULT_SIZE: PtySize = PtySize {
    rows: 40,
    cols: 120,
    pixel_width: 0,
    pixel_height: 0,
};
const PTY_OUTPUT_QUEUE_CAPACITY: usize = 64;

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

    async fn shutdown(&self) {
        let sessions: Vec<_> = {
            let mut sessions = self.sessions.lock().await;
            std::mem::take(&mut *sessions).into_values().collect()
        };
        for session in sessions {
            let mut session = session.lock().await;
            terminate_process(&mut *session).await;
            session.running = false;
        }
    }
}

async fn terminate_process(session: &mut Session) {
    // Kill the whole group first: a plain child kill (below) can leave a
    // shell's descendants — or a sandbox wrapper's shell — running.
    session.process_group.kill();
    match &mut session.process {
        Process::Pipe { child, .. } => {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        Process::Pty(pty) => {
            let _ = pty.child.kill();
        }
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
        "running": session.running,
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
        if let Some((success, exit_code)) = try_wait(session)? {
            session.running = false;
            let exit = format!("\n[process exited with code {}]", exit_code.unwrap_or(-1));
            append_output(session, exit.as_bytes());
            drain_process_output(session).await?;
            let denied_host = session
                .egress_invocation
                .as_ref()
                .and_then(crate::egress::EgressInvocation::take_denied_host);
            if let Some(error) = sandbox_denial(
                session.confined,
                success,
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
                is_error: !success,
                exit_code,
                attachments: Vec::new(),
            });
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if matches!(session.process, Process::Pty(_)) {
            drain_pty_output(session);
            tokio::time::sleep(remaining.min(Duration::from_millis(20))).await;
        } else {
            if let Some((stderr, count)) =
                read_pipe_or_wait(session, &mut stdout_buffer, &mut stderr_buffer, remaining)
                    .await?
            {
                if count > 0 {
                    if stderr {
                        append_stderr(session, &stderr_buffer[..count]);
                    } else {
                        append_output(session, &stdout_buffer[..count]);
                    }
                }
            }
        }
    }
    drain_pty_output(session);
    let body = output_for(session_id, session, max_tokens);
    Ok(ToolOutput {
        outcome: Default::default(),
        content: body.to_string(),
        is_error: false,
        exit_code: None,
        attachments: Vec::new(),
    })
}

fn try_wait(session: &mut Session) -> Result<Option<(bool, Option<i32>)>, ToolError> {
    let status = match &mut session.process {
        Process::Pipe { child, .. } => child
            .try_wait()?
            .map(|status| (status.success(), status.code())),
        Process::Pty(pty) => pty
            .child
            .try_wait()?
            .map(|status| (status.exit_code() == 0, Some(status.exit_code() as i32))),
    };
    Ok(status)
}

async fn drain_process_output(session: &mut Session) -> Result<(), ToolError> {
    if matches!(session.process, Process::Pty(_)) {
        for _ in 0..100 {
            drain_pty_output(session);
            let reader_done = match &session.process {
                Process::Pty(pty) => pty.reader_done.load(Ordering::Acquire),
                Process::Pipe { .. } => true,
            };
            if reader_done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        return Ok(());
    }
    let (stdout_tail, stderr_tail) = match &mut session.process {
        Process::Pipe { stdout, stderr, .. } => {
            let mut stdout_tail = Vec::new();
            stdout.read_to_end(&mut stdout_tail).await?;
            let mut stderr_tail = Vec::new();
            stderr.read_to_end(&mut stderr_tail).await?;
            (stdout_tail, stderr_tail)
        }
        Process::Pty(_) => unreachable!(),
    };
    append_output(session, &stdout_tail);
    append_stderr(session, &stderr_tail);
    Ok(())
}

fn drain_pty_output(session: &mut Session) {
    let mut chunks = Vec::new();
    if let Process::Pty(pty) = &mut session.process {
        while let Ok(bytes) = pty.output_rx.try_recv() {
            chunks.push(bytes);
        }
    }
    for bytes in chunks {
        // PTYs merge stdout and stderr. Keep the same bytes for the denial
        // classifier so shell-generated sandbox diagnostics remain visible.
        append_stderr(session, &bytes);
    }
}

async fn read_pipe_or_wait(
    session: &mut Session,
    stdout_buffer: &mut [u8],
    stderr_buffer: &mut [u8],
    wait: Duration,
) -> Result<Option<(bool, usize)>, std::io::Error> {
    let Process::Pipe { stdout, stderr, .. } = &mut session.process else {
        return Ok(None);
    };
    tokio::select! {
        result = stdout.read(stdout_buffer) => result.map(|count| Some((false, count))),
        result = stderr.read(stderr_buffer) => result.map(|count| Some((true, count))),
        _ = tokio::time::sleep(wait.min(Duration::from_millis(20))) => Ok(None),
    }
}

async fn write_process_input(process: &mut Process, chars: &str) -> Result<(), ToolError> {
    match process {
        Process::Pipe { stdin, .. } => stdin
            .as_mut()
            .ok_or_else(|| ToolError::Execution("shell stdin is closed".into()))?
            .write_all(chars.as_bytes())
            .await
            .map_err(ToolError::Io),
        Process::Pty(pty) => {
            let writer = Arc::clone(&pty.writer);
            let bytes = chars.as_bytes().to_vec();
            tokio::task::spawn_blocking(move || {
                let mut writer = writer
                    .lock()
                    .map_err(|_| std::io::Error::other("pty writer lock poisoned"))?;
                writer.write_all(&bytes)?;
                writer.flush()
            })
            .await
            .map_err(|error| ToolError::Execution(format!("pty input task failed: {error}")))?
            .map_err(ToolError::Io)
        }
    }
}

fn configure_pipe_environment(
    command: &mut Command,
    policy: &crate::sandbox::SandboxPolicy,
    command_egress: Option<&crate::sandbox::EgressGrant>,
    confined: bool,
    identity_dir: &std::path::Path,
) {
    for name in crate::builtins::PROVIDER_CREDENTIAL_ENV {
        command.env_remove(name);
    }
    for (name, value) in crate::sandbox::temp_env(policy) {
        command.env(name, value);
    }
    if confined {
        for name in crate::credentials::HOST_CREDENTIAL_ENV {
            command.env_remove(name);
        }
        for (name, value) in crate::sandbox::egress_env(policy) {
            command.env(name, value);
        }
        for (name, value) in crate::credentials::isolated_config_env(identity_dir) {
            command.env(name, value);
        }
        for (name, value) in crate::credentials::host_identity_env(command_egress, identity_dir) {
            command.env(name, value);
        }
        for (name, value) in policy.toolchain_env() {
            command.env(name, value);
        }
    }
}

fn configure_pty_environment(
    command: &mut CommandBuilder,
    policy: &crate::sandbox::SandboxPolicy,
    command_egress: Option<&crate::sandbox::EgressGrant>,
    confined: bool,
    identity_dir: &std::path::Path,
) {
    for name in crate::builtins::PROVIDER_CREDENTIAL_ENV {
        command.env_remove(name);
    }
    for (name, value) in crate::sandbox::temp_env(policy) {
        command.env(name, value);
    }
    if confined {
        for name in crate::credentials::HOST_CREDENTIAL_ENV {
            command.env_remove(name);
        }
        for (name, value) in crate::sandbox::egress_env(policy) {
            command.env(name, value);
        }
        for (name, value) in crate::credentials::isolated_config_env(identity_dir) {
            command.env(name, value);
        }
        for (name, value) in crate::credentials::host_identity_env(command_egress, identity_dir) {
            command.env(name, value);
        }
        for (name, value) in policy.toolchain_env() {
            command.env(name, value);
        }
    }
}

async fn start(
    sessions: &ExecSessionStore,
    ctx: &ToolContext,
    args: ExecCommandArgs,
) -> Result<ToolOutput, ToolError> {
    // These sessions outlive the turn that started them and `write_stdin`
    // feeds them afterwards, so confinement has to be applied here at spawn.
    // There is no way to sandbox the session later, and `write_stdin` is not
    // itself gated — a session that starts unconfined accepts arbitrary
    // commands unconfined for as long as it lives.
    let shell = args.shell.as_deref().unwrap_or("sh");
    let requested_confined = !ctx.unconfined_shell;
    if requested_confined {
        if let Err(unavailable) = crate::sandbox::availability() {
            return Err(ToolError::SandboxDenied {
                content: args.cmd.clone(),
                reason: unavailable.reason(),
                denied_host: None,
            });
        }
    }
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
    // A missing wrapper is allowed only for hosts where the process-wide CLI
    // sandbox probe already refused startup. If the host can sandbox but the
    // workspace policy could not be built, never silently downgrade.
    if requested_confined && wrapped.is_none() && crate::sandbox::availability().is_ok() {
        return Err(ToolError::Execution(format!(
            "refusing to run unconfined: no sandbox could be built for workspace {}",
            ctx.workspace_root.display()
        )));
    }
    let (program, command_args) =
        wrapped.unwrap_or_else(|| (shell.to_string(), vec!["-c".to_string(), args.cmd.clone()]));
    let identity_dir = ctx
        .session_tmp
        .as_ref()
        .map(|dir| dir.path().join("host-identity"))
        .unwrap_or_else(|| ctx.workspace_root.join(".forge-host-identity"));
    if confined {
        let _ = std::fs::create_dir_all(&identity_dir);
    }

    let (process, pgid) = if args.tty {
        let pty = native_pty_system()
            .openpty(PTY_DEFAULT_SIZE)
            .map_err(|error| ToolError::Execution(format!("failed to open exec PTY: {error}")))?;
        let mut command = CommandBuilder::new(&program);
        command.args(&command_args);
        command.cwd(&ctx.workspace_root);
        configure_pty_environment(
            &mut command,
            &policy,
            command_egress,
            confined,
            &identity_dir,
        );
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(|error| ToolError::Execution(format!("failed to spawn exec PTY: {error}")))?;
        // portable-pty calls `setsid` in the child, so its pid is also its
        // process-group id.
        let pgid = child.process_id().map(|pid| pid as i32);
        let mut reader = pty
            .master
            .try_clone_reader()
            .map_err(|error| ToolError::Execution(format!("failed to read exec PTY: {error}")))?;
        let writer = pty
            .master
            .take_writer()
            .map_err(|error| ToolError::Execution(format!("failed to write exec PTY: {error}")))?;
        let (output_tx, output_rx) = mpsc::sync_channel(PTY_OUTPUT_QUEUE_CAPACITY);
        let reader_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader_done_for_thread = Arc::clone(&reader_done);
        thread::Builder::new()
            .name("forge-exec-pty-reader".into())
            .spawn(move || {
                let mut buffer = [0_u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if output_tx.send(buffer[..count].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
                reader_done_for_thread.store(true, Ordering::Release);
            })
            .map_err(|error| {
                ToolError::Execution(format!("failed to start exec PTY reader: {error}"))
            })?;
        (
            Process::Pty(PtyProcess {
                _master: pty.master,
                writer: Arc::new(StdMutex::new(writer)),
                child,
                output_rx,
                reader_done,
            }),
            pgid,
        )
    } else {
        let mut command = Command::new(&program);
        command.args(&command_args);
        configure_pipe_environment(
            &mut command,
            &policy,
            command_egress,
            confined,
            &identity_dir,
        );
        set_process_group(&mut command);
        let mut child = command
            .current_dir(&ctx.workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let pgid = child.id().map(|pid| pid as i32);
        (
            Process::Pipe {
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
            },
            pgid,
        )
    };
    let session = Session {
        owner: ctx.session_id,
        command: args.cmd.clone(),
        shell: shell.to_string(),
        confined,
        workspace_root: ctx.workspace_root.clone(),
        egress_invocation,
        _session_tmp: ctx.session_tmp.clone(),
        process,
        process_group: ProcessGroupGuard::new(pgid),
        output: String::new(),
        stderr_output: String::new(),
        output_truncated: false,
        started: Instant::now(),
        running: true,
    };
    // A second guard local to this future. If the call is cancelled/dropped
    // while `collect` is still awaiting, dropping it reaps the tree even though
    // the session — and its own guard — lives on in the store.
    let mut cancel_guard = ProcessGroupGuard::new(pgid);
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
    // The store now owns the group; only an explicit terminate/shutdown may
    // kill a retained session.
    cancel_guard.disarm();
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
        "Start a sandboxed shell command with pipe or PTY output, retaining a session for polling or input"
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

    async fn shutdown(&self) {
        self.sessions.shutdown().await;
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
    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
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
        if session.owner != ctx.session_id {
            return Err(ToolError::Execution(format!(
                "shell session {} belongs to another session",
                args.session_id
            )));
        }
        if !args.chars.is_empty() {
            write_process_input(&mut session.process, &args.chars).await?;
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

    async fn shutdown(&self) {
        self.sessions.shutdown().await;
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
    async fn sessions_cannot_cross_session_owners() {
        let dir = tempdir().unwrap();
        let first_ctx = ToolContext::new(dir.path().to_path_buf())
            .with_session_id(forge_types::SessionId::new_v4());
        let second_ctx = ToolContext::new(dir.path().to_path_buf())
            .with_session_id(forge_types::SessionId::new_v4());
        let (exec_command, write_stdin) = unified_exec_tools();
        let first = exec_command
            .call(&first_ctx, json!({"cmd": "sleep 1", "yield_time_ms": 20}))
            .await
            .unwrap();
        let id = serde_json::from_str::<Value>(&first.content).unwrap()["session_id"]
            .as_u64()
            .unwrap();

        let error = write_stdin
            .call(&second_ctx, json!({"session_id": id, "yield_time_ms": 20}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("belongs to another session"));

        exec_command.shutdown().await;
    }

    #[tokio::test]
    async fn shutting_down_a_tool_registry_terminates_retained_shells() {
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf()).with_unconfined_shell();
        let (exec_command, write_stdin) = unified_exec_tools();
        let first = exec_command
            .call(&ctx, json!({"cmd": "sleep 30", "yield_time_ms": 20}))
            .await
            .unwrap();
        let id = serde_json::from_str::<Value>(&first.content).unwrap()["session_id"]
            .as_u64()
            .unwrap();

        exec_command.shutdown().await;

        let error = write_stdin
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

    #[test]
    fn exec_schema_does_not_advertise_login_shells() {
        let (exec_command, _) = unified_exec_tools();
        let schema = exec_command.input_schema();
        let properties = schema["properties"]
            .as_object()
            .expect("exec schema properties");
        assert!(!properties.contains_key("login"));
    }

    #[tokio::test]
    async fn tty_session_accepts_input_and_reports_merged_output() {
        if crate::sandbox::availability().is_err() {
            return;
        }
        let dir = tempdir().unwrap();
        let ctx = ToolContext::new(dir.path().to_path_buf());
        let (exec_command, write_stdin) = unified_exec_tools();
        let first = exec_command
            .call(
                &ctx,
                json!({
                    "cmd": "printf 'ready:'; read value; printf 'value=%s\\n' \"$value\"",
                    "tty": true,
                    "yield_time_ms": 100
                }),
            )
            .await
            .unwrap();
        let mut body: Value = serde_json::from_str(&first.content).unwrap();
        let id = body["session_id"].as_u64().expect("PTY session id");
        assert_eq!(body["running"], true);
        let finished = write_stdin
            .call(
                &ctx,
                json!({"session_id": id, "chars": "input\n", "yield_time_ms": 1_000}),
            )
            .await
            .unwrap();
        body = serde_json::from_str(&finished.content).unwrap();
        assert_eq!(body["running"], false, "{body}");
        assert!(body["output"]
            .as_str()
            .unwrap_or_default()
            .contains("value="));
    }

    #[tokio::test]
    async fn tty_session_stays_confined() {
        if crate::sandbox::availability().is_err() {
            return;
        }
        let workspace = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let target = outside.path().join("tty-escape.txt");
        let ctx = ToolContext::new(workspace.path().to_path_buf());
        let (exec_command, _) = unified_exec_tools();
        let error = exec_command
            .call(
                &ctx,
                json!({
                    "cmd": format!("printf escaped > {}", target.display()),
                    "tty": true,
                    "yield_time_ms": 1_000
                }),
            )
            .await
            .expect_err("a PTY sandbox denial must be surfaced");
        assert!(matches!(error, ToolError::SandboxDenied { .. }));
        assert!(!target.exists());
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

    /// Dropping the active `exec_command` call (Esc during the first yield)
    /// must reap the retained session's process tree. Without the process-group
    /// guard the store's child keeps running because `kill_on_drop` never fires.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_exec_command_kills_the_retained_session_tree() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join("marker");
        let ctx = ToolContext::new(dir.path().to_path_buf()).with_unconfined_shell();
        let (exec_command, _) = unified_exec_tools();
        let command = format!("true; sh -c 'sleep 2; printf done > {}'", marker.display());

        // A long yield keeps `collect` awaiting, so the timeout drops the call
        // future mid-flight — the same drop a cancelled turn performs.
        let dropped = tokio::time::timeout(
            Duration::from_millis(200),
            exec_command.call(
                &ctx,
                json!({"cmd": command, "yield_time_ms": 30_000}),
            ),
        )
        .await;
        assert!(dropped.is_err(), "session should still be running at drop");

        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(
            !marker.exists(),
            "cancelled exec session tree still executed its side effect"
        );
    }
}
