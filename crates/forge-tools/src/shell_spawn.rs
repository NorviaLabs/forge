//! One place that decides how a shell command is spawned.
//!
//! The bash tool (and background jobs, which run through it) and the
//! `exec_command` pipe and PTY sessions all spawn a shell under the same
//! rules: refuse when the sandbox is unavailable, start a per-call egress
//! gateway labelled with the tool call id, build the sandbox policy, wrap the
//! shell, strip provider and host credentials, and project the confined
//! environment. [`ShellSpawnSpec`] computes those decisions once; each mode
//! only turns the spec into its own command type and attaches its stdio.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::egress::EgressInvocation;
use crate::sandbox::EgressGrant;
use crate::ToolError;

/// How the spawned shell is driven. Only the refusal wording differs between
/// modes; every sandbox, egress and environment decision is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellSpawnMode {
    /// One-shot run with collected output: the bash tool and background jobs.
    Run,
    /// An `exec_command` session over pipes.
    Pipe,
    /// An `exec_command` session on a pseudo-terminal.
    Pty,
}

/// What the caller asks for.
#[derive(Debug, Clone, Copy)]
pub struct ShellSpawnRequest<'a> {
    pub mode: ShellSpawnMode,
    /// Shell executable (`bash` for the bash tool, the requested shell for
    /// `exec_command`).
    pub shell: &'a str,
    pub command: &'a str,
    pub workspace_root: &'a Path,
    /// The session's egress grant; `None` denies the network.
    pub egress: Option<&'a EgressGrant>,
    pub session_tmp: Option<&'a Path>,
    /// Whether confinement was requested. `false` is the explicitly approved
    /// unconfined path: no sandbox, no gateway, the real network.
    pub confined: bool,
    /// Label for the per-call egress gateway's events (the tool call id).
    pub egress_label: Option<&'a str>,
}

/// The spawn decisions for one shell command.
pub struct ShellSpawnSpec {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    confined: bool,
    env_remove: Vec<&'static str>,
    env_set: Vec<(String, OsString)>,
    egress_invocation: Option<EgressInvocation>,
}

impl ShellSpawnSpec {
    /// Decide how to spawn `request`.
    ///
    /// Confinement is applied at spawn because that is the only moment it can
    /// be: a process that starts unconfined stays unconfined for its whole
    /// life. An unavailable sandbox is surfaced as an approval-required denial
    /// instead of silently downgrading this invocation.
    pub async fn build(request: ShellSpawnRequest<'_>) -> Result<Self, ToolError> {
        let ShellSpawnRequest {
            mode,
            shell,
            command,
            workspace_root,
            egress,
            session_tmp,
            confined: requested_confined,
            egress_label,
        } = request;
        if requested_confined {
            if let Err(unavailable) = crate::sandbox::availability() {
                return Err(ToolError::SandboxDenied {
                    content: command.to_string(),
                    reason: unavailable.reason(),
                    denied_host: None,
                });
            }
        }
        let egress_invocation = if requested_confined {
            EgressInvocation::start_labeled(egress, egress_label)
                .await
                .map_err(|error| {
                    ToolError::Execution(format!(
                        "failed to start invocation egress proxy: {error}"
                    ))
                })?
        } else {
            None
        };
        let command_egress = egress_invocation
            .as_ref()
            .and_then(EgressInvocation::grant)
            .or(egress);
        let mut policy = crate::sandbox::SandboxPolicy::for_workspace(workspace_root)
            .with_command_access(command)
            .with_egress(command_egress);
        if crate::credentials::needs_git_writes(command) {
            policy = policy.with_git_writable();
        }
        if let Some(session_tmp) = session_tmp {
            policy = policy.with_session_tmp(session_tmp);
        }
        let wrapped = requested_confined
            .then(|| crate::sandbox::wrap_shell_command(shell, command, &policy))
            .flatten();

        // `wrap_shell_command` returns `None` for two very different reasons.
        // "This host has no sandbox" is a launch-time refusal in the supported
        // CLI. "This host has a sandbox but this policy could not be built" is
        // an anomaly — a workspace path that is not valid UTF-8, a root that
        // cannot be canonicalised — and nothing upstream knows it happened, so
        // falling back would drop confinement with no prompt and no message.
        // Refuse instead.
        if requested_confined && wrapped.is_none() && crate::sandbox::availability().is_ok() {
            return Err(ToolError::Execution(match mode {
                ShellSpawnMode::Run => format!(
                    "refusing to run unconfined: this host can sandbox, but no sandbox \
                     could be built for workspace {}",
                    workspace_root.display()
                ),
                ShellSpawnMode::Pipe | ShellSpawnMode::Pty => format!(
                    "refusing to run unconfined: no sandbox could be built for workspace {}",
                    workspace_root.display()
                ),
            }));
        }

        let confined = wrapped.is_some();
        let (program, args) =
            wrapped.unwrap_or_else(|| (shell.to_string(), vec!["-c".into(), command.to_string()]));

        // Do not use a login shell: `bash -l` sources profile files, which can
        // re-export credentials after the explicit removals below.
        let mut env_remove: Vec<&'static str> = crate::builtins::PROVIDER_CREDENTIAL_ENV.to_vec();
        let mut env_set: Vec<(String, OsString)> = crate::sandbox::shell_env(&policy);
        // The proxy env points at the in-sandbox relay (Linux) or the filtered
        // host proxy (macOS). An approved unconfined retry is supposed to use
        // the real network — HTTPS clients fail if we leave them talking to a
        // proxy that is either not listening or still deny-all.
        if confined {
            env_remove.extend_from_slice(crate::credentials::HOST_CREDENTIAL_ENV);
            let identity_dir = session_tmp
                .map(|dir| dir.join("host-identity"))
                .unwrap_or_else(|| workspace_root.join(".forge-host-identity"));
            let _ = std::fs::create_dir_all(&identity_dir);
            let confined_env = crate::sandbox::egress_env(&policy)
                .into_iter()
                .chain(crate::credentials::isolated_config_env(&identity_dir))
                .chain(crate::credentials::host_identity_env(
                    command_egress,
                    &identity_dir,
                ))
                .chain(policy.toolchain_env());
            env_set.extend(confined_env.map(|(name, value)| (name, OsString::from(value))));
        }

        Ok(Self {
            program,
            args,
            cwd: workspace_root.to_path_buf(),
            confined,
            env_remove,
            env_set,
            egress_invocation,
        })
    }

    /// Whether the command runs inside the sandbox.
    pub fn confined(&self) -> bool {
        self.confined
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Variables removed from the inherited environment.
    pub fn env_remove(&self) -> &[&'static str] {
        &self.env_remove
    }

    /// Variables set on top of the inherited environment, in order.
    pub fn env_set(&self) -> &[(String, OsString)] {
        &self.env_set
    }

    /// The per-call egress gateway, when the command is confined. It must
    /// outlive the process: dropping it stops the listener.
    pub fn egress_invocation(&self) -> Option<&EgressInvocation> {
        self.egress_invocation.as_ref()
    }

    pub fn take_egress_invocation(&mut self) -> Option<EgressInvocation> {
        self.egress_invocation.take()
    }

    /// The command for the run and pipe modes. The caller attaches stdio and
    /// the process group.
    pub fn tokio_command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(&self.args).current_dir(&self.cwd);
        for name in &self.env_remove {
            command.env_remove(name);
        }
        for (name, value) in &self.env_set {
            command.env(name, value);
        }
        command
    }

    /// The command for the PTY mode.
    pub fn pty_command(&self) -> portable_pty::CommandBuilder {
        let mut command = portable_pty::CommandBuilder::new(&self.program);
        command.args(&self.args);
        command.cwd(&self.cwd);
        for name in &self.env_remove {
            command.env_remove(name);
        }
        for (name, value) in &self.env_set {
            command.env(name, value);
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(
        mode: ShellSpawnMode,
        workspace: &'a Path,
        tmp: &'a Path,
        confined: bool,
    ) -> ShellSpawnRequest<'a> {
        ShellSpawnRequest {
            mode,
            shell: "bash",
            command: "git push origin main",
            workspace_root: workspace,
            egress: None,
            session_tmp: Some(tmp),
            confined,
            egress_label: Some("call-1"),
        }
    }

    /// Everything a mode's spawn depends on, minus the gateway's socket
    /// paths, which are fresh per invocation.
    fn decisions(spec: &ShellSpawnSpec) -> (bool, Vec<&'static str>, Vec<String>) {
        let names = spec
            .env_set()
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        (spec.confined(), spec.env_remove().to_vec(), names)
    }

    #[tokio::test]
    async fn the_three_modes_make_the_same_sandbox_and_env_decisions() {
        let workspace = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        for mode in [
            ShellSpawnMode::Run,
            ShellSpawnMode::Pipe,
            ShellSpawnMode::Pty,
        ] {
            match ShellSpawnSpec::build(request(mode, workspace.path(), tmp.path(), true)).await {
                Ok(spec) => {
                    assert!(spec.confined(), "{mode:?}: a confined request is wrapped");
                    // No session grant means the network is denied, so
                    // there is no template to start a gateway from.
                    assert!(spec.egress_invocation().is_none(), "{mode:?}");
                    for name in crate::builtins::PROVIDER_CREDENTIAL_ENV
                        .iter()
                        .chain(crate::credentials::HOST_CREDENTIAL_ENV)
                    {
                        assert!(spec.env_remove().contains(name), "{mode:?}: {name}");
                    }
                    let (_, _, names) = decisions(&spec);
                    for name in [
                        "TMPDIR",
                        "XDG_CONFIG_HOME",
                        "GIT_CONFIG_GLOBAL",
                        "CARGO_HOME",
                    ] {
                        assert!(names.iter().any(|n| n == name), "{mode:?}: {name}");
                    }
                    seen.push(decisions(&spec));
                }
                // A host without a sandbox refuses every mode the same way.
                Err(ToolError::SandboxDenied { denied_host, .. }) => {
                    assert!(crate::sandbox::availability().is_err());
                    assert_eq!(denied_host, None);
                }
                Err(other) => panic!("{mode:?}: {other}"),
            }
        }
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    }

    /// With a live session grant every confined mode starts its own gateway
    /// labelled with the tool call id, and the proxy env points at it.
    #[tokio::test]
    async fn a_confined_spawn_gets_a_gateway_labelled_with_the_call_id() {
        if crate::sandbox::availability().is_err() {
            return;
        }
        let proxy =
            match crate::egress::EgressProxy::start(crate::egress::EgressPolicy::new()).await {
                Ok(proxy) => proxy,
                Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
                Err(error) => panic!("egress proxy failed to start: {error}"),
            };
        let dir = tempfile::tempdir().unwrap();
        let template = EgressGrant {
            proxy_port: proxy.addr().port(),
            socket_path: dir.path().join("session.sock"),
            control: Some(proxy.shared().clone()),
        };
        let workspace = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        for mode in [
            ShellSpawnMode::Run,
            ShellSpawnMode::Pipe,
            ShellSpawnMode::Pty,
        ] {
            let mut req = request(mode, workspace.path(), tmp.path(), true);
            req.egress = Some(&template);
            let mut spec = ShellSpawnSpec::build(req).await.unwrap();
            let names = decisions(&spec).2;
            assert!(names.iter().any(|n| n == "HTTPS_PROXY"), "{mode:?}");
            let invocation = spec.take_egress_invocation().expect("gateway");
            assert_eq!(invocation.label(), Some("call-1"), "{mode:?}");
            seen.push(names);
        }
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    }

    #[tokio::test]
    async fn the_unconfined_path_runs_the_plain_shell_without_a_gateway() {
        let workspace = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut seen = Vec::new();
        for mode in [
            ShellSpawnMode::Run,
            ShellSpawnMode::Pipe,
            ShellSpawnMode::Pty,
        ] {
            let spec = ShellSpawnSpec::build(request(mode, workspace.path(), tmp.path(), false))
                .await
                .unwrap();
            assert!(!spec.confined());
            assert!(spec.egress_invocation().is_none());
            assert_eq!(spec.program(), "bash");
            assert_eq!(spec.args(), ["-c", "git push origin main"]);
            // Provider keys never reach a shell; host credentials and the
            // proxy env are left alone so the approved run uses the real
            // network and the operator's identity.
            assert_eq!(spec.env_remove(), crate::builtins::PROVIDER_CREDENTIAL_ENV);
            let (_, _, names) = decisions(&spec);
            assert!(!names
                .iter()
                .any(|n| n.ends_with("_PROXY") || n == "GIT_CONFIG_GLOBAL"));
            assert!(names.iter().any(|n| n == "TMPDIR"));
            seen.push(decisions(&spec));
        }
        assert!(seen.windows(2).all(|pair| pair[0] == pair[1]), "{seen:?}");
    }

    #[tokio::test]
    async fn the_built_commands_carry_the_spec_environment() {
        let workspace = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let spec = ShellSpawnSpec::build(request(
            ShellSpawnMode::Pipe,
            workspace.path(),
            tmp.path(),
            false,
        ))
        .await
        .unwrap();
        let pipe = spec.tokio_command();
        let envs: Vec<_> = pipe.as_std().get_envs().collect();
        for name in spec.env_remove() {
            assert!(envs.contains(&(std::ffi::OsStr::new(name), None)), "{name}");
        }
        for (name, value) in spec.env_set() {
            assert!(
                envs.contains(&(std::ffi::OsStr::new(name), Some(value.as_os_str()))),
                "{name}"
            );
        }
        assert_eq!(pipe.as_std().get_current_dir(), Some(workspace.path()));
        let pty = spec.pty_command();
        for (name, value) in spec.env_set() {
            assert_eq!(pty.get_env(name), Some(value.as_os_str()), "{name}");
        }
        for name in spec.env_remove() {
            assert_eq!(pty.get_env(name), None, "{name}");
        }
    }
}
