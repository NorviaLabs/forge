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
    /// Credential env names from the session's loaded profiles, stripped in
    /// addition to the built-in vendor aliases and `FORGE_*_API_KEY`.
    pub credential_env_names: &'a [String],
}

/// The spawn decisions for one shell command.
pub struct ShellSpawnSpec {
    program: String,
    args: Vec<String>,
    cwd: PathBuf,
    confined: bool,
    env_remove: Vec<String>,
    env_set: Vec<(String, OsString)>,
    egress_invocation: Option<EgressInvocation>,
}

/// Start the SSH broker for one confined invocation, and the
/// `GIT_SSH_COMMAND` that reaches it. `None` when it cannot be started; the
/// command then runs with no SSH, as it did before the broker existed.
pub(crate) async fn start_ssh_broker(
    invocation: &EgressInvocation,
    credential_env_names: &[String],
) -> Option<(crate::ssh_broker::SshBroker, String)> {
    let exe = crate::git_network::helper_exe().ok()?;
    let connect_addr = invocation.tcp_addr().to_string();
    let names: std::sync::Arc<[String]> = credential_env_names.into();
    let compose: crate::ssh_broker::Compose = std::sync::Arc::new(move || {
        let (names, connect_addr) = (std::sync::Arc::clone(&names), connect_addr.clone());
        Box::pin(async move { crate::git_network::host_ssh_command(&names, &connect_addr).await })
    });
    // A push changes the remote under the user's identity: it goes through
    // when its target is recorded for the project, or the user allows it.
    let shared = invocation.shared().clone();
    let authorize: crate::ssh_broker::Authorize = std::sync::Arc::new(move |request| {
        let shared = shared.clone();
        Box::pin(async move {
            if !request.is_push() {
                return Ok(());
            }
            let target = crate::write_gate::target_for_ssh_push(request.host(), &request.command);
            let write = crate::write_gate::Write {
                summary: format!("git push to {target}"),
                target,
            };
            match shared.admit_write(&write).await {
                crate::egress::Admission::Allowed => Ok(()),
                crate::egress::Admission::RefusedByUser => {
                    Err(format!("the user refused the push to {}", write.target))
                }
                crate::egress::Admission::Refused => Err(format!(
                    "{}: pushing to {} was not approved",
                    crate::egress::SANDBOX_DENIED_REASON,
                    write.target
                )),
            }
        })
    });
    let broker = crate::ssh_broker::SshBroker::start(compose, authorize)
        .await
        .ok()?;
    let git_ssh_command = broker.git_ssh_command(&exe);
    Some((broker, git_ssh_command))
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
            credential_env_names,
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
        let mut egress_invocation = if requested_confined {
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
        // Git over SSH from inside the sandbox: the keys stay outside, so
        // git is pointed at a broker that runs the real ssh on the host,
        // through this invocation's gateway. Without it, an SSH remote
        // could only work by running the whole command unconfined.
        let ssh_broker = match &egress_invocation {
            Some(invocation) => start_ssh_broker(invocation, credential_env_names).await,
            None => None,
        };
        // Signing, the same way: the key stays outside and a broker signs
        // what git asks it to. Its wrapper scripts live with the spawn's
        // other git files.
        let identity_dir = session_tmp
            .map(|dir| dir.join("host-identity"))
            .unwrap_or_else(|| workspace_root.join(".forge-host-identity"));
        let sign_broker = if requested_confined && egress_invocation.is_some() {
            let _ = std::fs::create_dir_all(&identity_dir);
            crate::sign_broker::start_for_repo(workspace_root, &identity_dir).await
        } else {
            None
        };
        let command_egress = egress_invocation
            .as_ref()
            .and_then(EgressInvocation::grant)
            .or(egress);
        use crate::sandbox::WithEgress as _;
        // The whole git dir is writable, hooks and config included: no git
        // outside the sandbox runs on this repository, so whatever a
        // command puts there runs confined too. A change is reported with
        // the call that made it.
        let mut policy = crate::sandbox::SandboxPolicy::for_workspace(workspace_root)
            .with_command_access(command)
            .with_egress(command_egress)
            .with_git_open();
        // The git proxy and the SSH and signing brokers run Forge's own
        // binary inside the sandbox; on Linux it is masked when installed
        // under `/home`.
        if let Ok(exe) = crate::git_network::helper_exe() {
            policy = policy.with_executable(exe);
        }
        // A remote that is another repository on this disk is read through
        // the file system, not the network. On Linux it is masked when it
        // lives under `/home`, as Forge's own git already accounts for.
        // (macOS reads everything outside a deny-list, so this is Linux only.)
        if cfg!(target_os = "linux") {
            for remote in forge_sandbox::git::local_remote_repositories(workspace_root) {
                if let Ok(next) = policy.clone().with_read_path(&remote) {
                    policy = next;
                }
            }
        }
        // Directories the user let sandboxed commands write (D8).
        for dir in crate::write_grants::write_grants_for(workspace_root) {
            policy = policy.with_write_path(dir);
        }
        if let Some(session_tmp) = session_tmp {
            policy = policy.with_session_tmp(session_tmp);
        }
        if let Some((broker, _)) = &ssh_broker {
            policy = policy.with_loopback_port(broker.addr().port());
        }
        if let Some((broker, _)) = &sign_broker {
            policy = policy.with_loopback_port(broker.addr().port());
        }
        if requested_confined {
            if let Some(cache) = crate::sandbox::SandboxPolicy::default_tool_cache() {
                policy = policy.with_tool_cache(cache);
            }
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
        let mut env_remove = crate::builtins::provider_credential_env_names(credential_env_names);
        let mut env_set: Vec<(String, OsString)> = crate::sandbox::shell_env(&policy);
        // The proxy env points at the in-sandbox relay (Linux) or the filtered
        // host proxy (macOS). An approved unconfined retry is supposed to use
        // the real network — HTTPS clients fail if we leave them talking to a
        // proxy that is either not listening or still deny-all.
        if confined {
            env_remove.extend(
                crate::credentials::HOST_CREDENTIAL_ENV
                    .iter()
                    .map(|name| (*name).to_string()),
            );
            let _ = std::fs::create_dir_all(&identity_dir);
            let mut git_env = crate::credentials::host_identity_env(
                command_egress,
                &identity_dir,
                workspace_root,
            );
            if let Some((_, config)) = &sign_broker {
                crate::sign_broker::push_git_config(&mut git_env, config.clone());
            }
            // The user's own `http.proxy` would outrank the proxy
            // environment and point git past the gateway, at an address the
            // sandbox does not let it reach.
            if let Some(invocation) = &egress_invocation {
                crate::sign_broker::push_git_config(
                    &mut git_env,
                    vec![(
                        "http.proxy".into(),
                        format!("http://{}", invocation.tcp_addr()),
                    )],
                );
            }
            // git:// has no proxy support of its own; without this it has
            // no route out of the sandbox at all.
            #[cfg(unix)]
            if let Some(invocation) = &egress_invocation {
                let wrapper = crate::git_network::helper_exe().ok().and_then(|exe| {
                    crate::git_network::write_git_proxy_wrapper(
                        &identity_dir,
                        &exe,
                        &invocation.tcp_addr().to_string(),
                    )
                    .ok()
                });
                if let Some(wrapper) = wrapper {
                    crate::sign_broker::push_git_config(
                        &mut git_env,
                        vec![("core.gitProxy".into(), wrapper)],
                    );
                }
            }
            let confined_env = crate::sandbox::egress_env(&policy)
                .into_iter()
                .chain(crate::credentials::isolated_config_env(&identity_dir))
                .chain(git_env)
                .chain(policy.toolchain_env());
            env_set.extend(confined_env.map(|(name, value)| (name, OsString::from(value))));
        }
        if let (Some((broker, _)), Some(invocation)) = (sign_broker, egress_invocation.as_mut()) {
            invocation.hold(broker);
        }
        if let Some((broker, git_ssh_command)) = ssh_broker {
            if confined {
                env_set.push(("GIT_SSH_COMMAND".into(), git_ssh_command.into()));
                // An explicit variant: git must not probe the client with
                // `-G` to find out what it is.
                env_set.push(("GIT_SSH_VARIANT".into(), "ssh".into()));
            }
            // The broker lives as long as the invocation's gateway does.
            if let Some(invocation) = egress_invocation.as_mut() {
                invocation.hold(broker);
            }
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
    pub fn env_remove(&self) -> &[String] {
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
            credential_env_names: &[],
        }
    }

    /// Everything a mode's spawn depends on, minus the gateway's socket
    /// paths, which are fresh per invocation.
    fn decisions(spec: &ShellSpawnSpec) -> (bool, Vec<String>, Vec<String>) {
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
                        assert!(
                            spec.env_remove().iter().any(|removed| removed == name),
                            "{mode:?}: {name}"
                        );
                    }
                    let (_, _, names) = decisions(&spec);
                    for name in [
                        "TMPDIR",
                        "XDG_CONFIG_HOME",
                        "GIT_CONFIG_COUNT",
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
    #[cfg_attr(
        target_os = "linux",
        ignore = "Linux has no relay from the sandbox to host-loopback brokers and gateways yet"
    )]
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
            // Git's SSH goes to the broker, and the sandbox can reach it.
            let broker = spec
                .env_set()
                .iter()
                .find(|(name, _)| name == "GIT_SSH_COMMAND")
                .map(|(_, value)| value.to_string_lossy().into_owned())
                .expect("a confined command with a gateway gets the SSH broker");
            assert!(broker.contains(crate::ssh_broker::SUBCOMMAND), "{broker}");
            let port = broker
                .split_whitespace()
                .find_map(|word| word.strip_prefix("127.0.0.1:"))
                .expect("the broker address");
            assert!(
                spec.args()[1].contains(&format!("localhost:{port}")),
                "the profile must let the command reach the broker"
            );
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
            let fixed: Vec<String> = crate::builtins::PROVIDER_CREDENTIAL_ENV
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            assert_eq!(&spec.env_remove()[..fixed.len()], fixed.as_slice());
            assert!(spec
                .env_remove()
                .iter()
                .all(|name| crate::builtins::is_provider_credential_env(name, &[])));
            let (_, _, names) = decisions(&spec);
            assert!(!names
                .iter()
                .any(|n| n.ends_with("_PROXY") || n == "GIT_CONFIG_COUNT"));
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
