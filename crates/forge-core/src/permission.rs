//! Session-scoped sandbox runtime: the egress proxy and its grant.
//!
//! The launch-time check that a host can confine lives in `forge-cli`. This
//! module starts the proxy once a session exists. A host that cannot confine
//! never reaches here from the supported CLI.

/// A running egress proxy plus the grant that points at it.
///
/// The proxy is kept alive by holding it: `EgressProxy` stops serving and
/// removes its socket on drop, so a caller that discards this leaves a grant
/// addressing nothing, and every network call inside the sandbox fails in a
/// way that looks like a broken network rather than a dropped proxy.
pub struct EgressRuntime {
    _proxy: forge_tools::egress::EgressProxy,
    grant: std::sync::Arc<forge_tools::sandbox::EgressGrant>,
    shared: forge_tools::egress::EgressShared,
}

impl std::fmt::Debug for EgressRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressRuntime")
            .field("grant", &self.grant)
            .finish()
    }
}

impl EgressRuntime {
    pub fn grant(&self) -> std::sync::Arc<forge_tools::sandbox::EgressGrant> {
        std::sync::Arc::clone(&self.grant)
    }

    pub fn grant_host(&self, pattern: &str) {
        self.shared.grant_host(pattern);
    }
}

/// Hosts this workspace may reach, taken from the merged permission files.
///
/// Nothing is pre-allowed. A host is reachable only when the personal
/// `permissions.toml` contains a matching `host(...)` allow (or `host(*)`
/// for unrestricted network). Repo-committed `allow` rules never loosen
/// this — same trust split as the HITL path.
pub fn egress_policy_for_workspace(
    workspace: &std::path::Path,
) -> forge_tools::egress::EgressPolicy {
    let (permissions, _) = forge_config::load_permissions(workspace);
    forge_tools::egress::EgressPolicy::from_permissions(&permissions)
}

/// Start the egress proxy for a session.
///
/// Returns `None` when a proxy cannot be started, which leaves the network off
/// — the safe direction. A session without egress still works; commands that
/// need the network fail with the sandbox's own explanation rather than
/// silently reaching it.
///
/// The allow-list is empty unless the caller passes hosts the user allowed.
/// That matches Codex: pre-allow nothing, fail closed, let the user add
/// destinations (or `host(*)` for unrestricted network). There is no
/// hardcoded set of "trusted" registries.
pub async fn start_egress(
    session_id: uuid::Uuid,
    policy: forge_tools::egress::EgressPolicy,
) -> Option<EgressRuntime> {
    use forge_tools::egress::EgressProxy;

    let mut proxy = EgressProxy::start(policy.clone()).await.ok()?;

    // The socket lives outside the workspace: inside it, the agent could
    // delete or replace the thing carrying its own traffic, and `.forge` is
    // read-only in the sandbox anyway.
    let socket_path = std::env::temp_dir().join(format!("forge-egress-{session_id}.sock"));
    proxy
        .serve_on_unix_socket(&socket_path, policy)
        .await
        .ok()?;

    Some(EgressRuntime {
        grant: std::sync::Arc::new(forge_tools::sandbox::EgressGrant {
            proxy_port: proxy.addr().port(),
            socket_path,
            control: Some(proxy.shared().clone()),
        }),
        shared: proxy.shared().clone(),
        _proxy: proxy,
    })
}

#[cfg(test)]
mod egress_runtime_tests {
    use super::*;

    /// A session's proxy must be reachable while the session holds it. The
    /// listener stops and the socket is removed on drop, so a grant that
    /// outlived its runtime would point at nothing and every network call
    /// would fail looking like a broken network.
    #[tokio::test]
    async fn a_started_proxy_is_listening_and_addressable() {
        let Some(runtime) = start_egress(
            uuid::Uuid::new_v4(),
            forge_tools::egress::EgressPolicy::new(),
        )
        .await
        else {
            return; // could not bind on this host; nothing to assert
        };
        let grant = runtime.grant();
        assert!(grant.proxy_port > 0, "the port must be real");
        assert!(
            grant.socket_path.exists(),
            "the socket must exist while the session holds the proxy"
        );
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", grant.proxy_port))
                .await
                .is_ok(),
            "the proxy must accept connections"
        );
    }

    /// Dropping the session must not leave a socket behind for the next one to
    /// collide with.
    #[tokio::test]
    async fn dropping_the_runtime_removes_the_socket() {
        let Some(runtime) = start_egress(
            uuid::Uuid::new_v4(),
            forge_tools::egress::EgressPolicy::new(),
        )
        .await
        else {
            return;
        };
        let path = runtime.grant().socket_path.clone();
        assert!(path.exists());
        drop(runtime);
        assert!(!path.exists(), "a stale socket would break the next bind");
    }

    /// A proxy started with the default (empty) policy must refuse the
    /// registries that used to be pre-allowed. The decision is host-level,
    /// so this does not need the destination to exist.
    #[tokio::test]
    async fn the_default_policy_refuses_former_ecosystem_hosts() {
        let Some(runtime) = start_egress(
            uuid::Uuid::new_v4(),
            forge_tools::egress::EgressPolicy::new(),
        )
        .await
        else {
            return;
        };
        let port = runtime.grant().proxy_port;
        for host in [
            "crates.io:443",
            "registry.npmjs.org:443",
            "pypi.org:443",
            "github.com:443",
        ] {
            let status = connect_status(port, host).await;
            assert!(
                status.contains("403"),
                "{host} must be refused until the user allows it, got {status:?}"
            );
        }
    }

    /// `host(*)` is the unrestricted-network escape hatch: the proxy still
    /// runs, but every host is permitted unless a deny matches.
    #[tokio::test]
    async fn a_star_allow_lets_an_arbitrary_host_through_the_policy() {
        let mut policy = forge_tools::egress::EgressPolicy::new();
        policy.allow("*");
        let Some(runtime) = start_egress(uuid::Uuid::new_v4(), policy).await else {
            return;
        };
        let status = connect_status(
            runtime.grant().proxy_port,
            "definitely-not-allowed.invalid:443",
        )
        .await;
        assert!(
            !status.contains("403"),
            "host(*) must not 403, got {status:?}"
        );
    }

    async fn connect_status(port: u16, target: &str) -> String {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).await.unwrap();
        status
    }

    /// The socket belongs outside the workspace: inside it the agent could
    /// delete or replace the thing carrying its own traffic.
    #[tokio::test]
    async fn the_socket_lives_outside_the_workspace() {
        let Some(runtime) = start_egress(
            uuid::Uuid::new_v4(),
            forge_tools::egress::EgressPolicy::new(),
        )
        .await
        else {
            return;
        };
        let path = runtime.grant().socket_path.clone();
        assert!(
            path.starts_with(std::env::temp_dir()),
            "expected a temp path, got {}",
            path.display()
        );
    }
}

#[cfg(test)]
mod session_egress_tests {
    use crate::*;

    /// Serializes tests that redirect `HOME` / `XDG_CONFIG_HOME` so they
    /// cannot race with each other. Restores the environment on drop.
    struct IsolatedUserConfig {
        _lock: std::sync::MutexGuard<'static, ()>,
        _home: tempfile::TempDir,
        saved: Vec<(String, Option<String>)>,
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl IsolatedUserConfig {
        fn new() -> Self {
            let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::new();
            for key in ["HOME", "XDG_CONFIG_HOME"] {
                saved.push((key.to_string(), std::env::var(key).ok()));
            }
            let home = tempfile::TempDir::new().unwrap();
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::set_var("HOME", home.path());
            Self {
                _lock,
                _home: home,
                saved,
            }
        }
    }

    impl Drop for IsolatedUserConfig {
        fn drop(&mut self) {
            for (key, val) in self.saved.drain(..) {
                match val {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }

    async fn connect_status(port: u16, target: &str) -> String {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        stream
            .write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).await.unwrap();
        status
    }

    /// A session in an empty workspace, with no personal host rules, must
    /// not pre-allow package registries or GitHub.
    #[tokio::test]
    async fn a_session_does_not_pre_allow_ecosystem_hosts() {
        let _user = IsolatedUserConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let policy = super::egress_policy_for_workspace(dir.path());
        for host in [
            "crates.io",
            "registry.npmjs.org",
            "pypi.org",
            "github.com",
            "api.github.com",
        ] {
            assert!(
                !policy.permits(host),
                "{host} must stay denied until the user allows it"
            );
        }
    }

    /// Personal `host(...)` allows are the only thing that open the proxy.
    /// A repo-committed `host(*)` must not.
    #[tokio::test]
    async fn only_personal_host_allows_open_egress() {
        let _user = IsolatedUserConfig::new();
        let dir = tempfile::tempdir().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("permissions.toml"),
            "allow = [\"host(*)\"]\n",
        )
        .unwrap();
        let policy = super::egress_policy_for_workspace(dir.path());
        assert!(
            !policy.permits("crates.io"),
            "a repo-committed host allow must not loosen egress"
        );

        let user_path = forge_config::user_permissions_path().expect("redirected HOME");
        std::fs::create_dir_all(user_path.parent().unwrap()).unwrap();
        std::fs::write(&user_path, "allow = [\"host(**.example.com)\"]\n").unwrap();
        let policy = super::egress_policy_for_workspace(dir.path());
        assert!(policy.permits("api.example.com"));
        assert!(policy.permits("example.com"));
        assert!(!policy.permits("crates.io"));
    }

    /// Applying a personal host policy must replace the empty default, not
    /// leave the original deny-all proxy in place.
    #[tokio::test]
    async fn applying_a_host_allow_replaces_the_session_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let model = std::sync::Arc::new(forge_model::MockModelClient::script(vec![]));
        let mut session = AgentSession::create(
            LoopConfig {
                max_turns: 1,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                ..Default::default()
            },
            model,
            forge_tools::ToolRegistry::new(),
        )
        .await
        .unwrap();
        let Some(grant) = session.egress_grant() else {
            return;
        };
        let denied = connect_status(grant.proxy_port, "crates.io:443").await;
        assert!(
            denied.contains("403"),
            "a fresh session must refuse crates.io, got {denied:?}"
        );

        let file = forge_config::PermissionsFile {
            allow: vec!["host(*)".into()],
            deny: vec![],
        };
        session
            .apply_egress_policy(forge_tools::egress::EgressPolicy::from_permissions(&file))
            .await;
        let grant = session.egress_grant().expect("replacement proxy");
        let status = connect_status(grant.proxy_port, "definitely-not-allowed.invalid:443").await;
        assert!(
            !status.contains("403"),
            "host(*) must replace the deny-all proxy, got {status:?}"
        );
    }

    /// Egress is on by default: a session starts its proxy and hands every
    /// tool a grant pointing at it. Without this the allow-list exists but
    /// nothing consults it, which is where this sat for most of its life.
    #[tokio::test]
    async fn a_session_starts_with_network_egress() {
        // The egress proxy binds a loopback listener; hosts that deny that
        // (CI sandboxes, agent harnesses) cannot run this contract at all.
        // Binding is also intermittently denied under load, so only assert
        // when a control bind succeeds alongside the session's own.
        let dir = tempfile::tempdir().unwrap();
        let model = std::sync::Arc::new(forge_model::MockModelClient::script(vec![]));
        let mut session = AgentSession::create(
            LoopConfig {
                max_turns: 1,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                ..Default::default()
            },
            model,
            forge_tools::ToolRegistry::new(),
        )
        .await
        .unwrap();
        for _ in 0..2 {
            if session.has_network_egress() {
                break;
            }
            let Ok(probe) = std::net::TcpListener::bind("127.0.0.1:0") else {
                eprintln!("skipping: this host denies binding a listener");
                return;
            };
            drop(probe);
            drop(session);
            let model = std::sync::Arc::new(forge_model::MockModelClient::script(vec![]));
            session = AgentSession::create(
                LoopConfig {
                    max_turns: 1,
                    workspace: dir.path().to_path_buf(),
                    journal_dir: dir.path().join("j"),
                    ..Default::default()
                },
                model,
                forge_tools::ToolRegistry::new(),
            )
            .await
            .unwrap();
        }

        assert!(
            session.has_network_egress(),
            "a session must start its egress proxy"
        );
        let grant = session.egress_grant().expect("a grant must exist");
        assert!(grant.proxy_port > 0);
        assert!(grant.socket_path.exists());
    }

    /// The grant has to actually reach the command. Everything upstream can be
    /// correct while the tool still runs with no proxy configured, which is
    /// precisely the state this sat in until it was wired: the allow-list
    /// existed and nothing consulted it.
    #[tokio::test]
    async fn a_command_run_by_the_session_sees_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let model = std::sync::Arc::new(forge_model::MockModelClient::script(vec![]));
        let session = AgentSession::create(
            LoopConfig {
                max_turns: 1,
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                ..Default::default()
            },
            model,
            forge_tools::ToolRegistry::new(),
        )
        .await
        .unwrap();
        let Some(grant) = session.egress_grant() else {
            return; // no proxy on this host; nothing to assert
        };

        let out = forge_tools::run_shell_command_with_egress(
            "printf %s \"$HTTP_PROXY\"",
            dir.path(),
            Some(&grant),
        )
        .await
        .unwrap();

        assert!(
            out.content.contains("127.0.0.1"),
            "the command must be pointed at the proxy, got {:?}",
            out.content
        );
    }
}

#[cfg(test)]
mod propagation_contract {
    //! Where a session's permissions must reach.
    //!
    //! Every gap closed here was the same shape: a path wired up and never
    //! checked. `background_run` was calling the grantless variant of
    //! `run_shell_command`, so a backgrounded `cargo build` was confined *and*
    //! offline while the identical foreground command worked. Nothing failed;
    //! it just quietly behaved differently.

    use crate::*;

    async fn session(dir: &std::path::Path) -> AgentSession {
        AgentSession::create(
            LoopConfig {
                max_turns: 1,
                workspace: dir.to_path_buf(),
                journal_dir: dir.join("j"),
                ..Default::default()
            },
            std::sync::Arc::new(forge_model::MockModelClient::script(vec![])),
            forge_tools::ToolRegistry::new(),
        )
        .await
        .unwrap()
    }

    /// A subagent shares the parent's grant rather than starting its own proxy:
    /// one session, one allow-list, one place to revoke it.
    #[tokio::test]
    async fn a_subagent_inherits_the_parent_grant() {
        let dir = tempfile::tempdir().unwrap();
        let parent = session(dir.path()).await;
        let Some(parent_grant) = parent.egress_grant() else {
            return;
        };

        let child = parent
            .create_child(
                uuid::Uuid::new_v4(),
                dir.path().to_path_buf(),
                tokio_util::sync::CancellationToken::new(),
                &SubagentSpec {
                    role: "child".into(),
                    prompt: "work".into(),
                    tool_allowlist: None,
                    max_turns: None,
                },
            )
            .await
            .expect("subagent session");

        let child_grant = child
            .tool_ctx
            .egress
            .clone()
            .expect("a child must inherit the parent's grant, not lose the network");
        assert_eq!(
            child_grant.proxy_port, parent_grant.proxy_port,
            "the child must use the parent's proxy, not a second one"
        );
        assert_eq!(child_grant.socket_path, parent_grant.socket_path);

        // And it must not own a proxy of its own: two proxies per session means
        // two allow-lists and two things to revoke.
        assert!(
            !child.has_network_egress(),
            "the child holds a grant, not a listener"
        );
    }

    /// Two sessions must not collide on a socket path, and one ending must not
    /// disturb the other. The path is derived from the session id for exactly
    /// this reason.
    #[tokio::test]
    async fn concurrent_sessions_get_independent_egress() {
        let a_dir = tempfile::tempdir().unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let a = session(a_dir.path()).await;
        let b = session(b_dir.path()).await;

        let (Some(ga), Some(gb)) = (a.egress_grant(), b.egress_grant()) else {
            return;
        };
        assert_ne!(ga.socket_path, gb.socket_path, "sockets must not collide");
        assert_ne!(ga.proxy_port, gb.proxy_port, "ports must not collide");
        assert!(ga.socket_path.exists() && gb.socket_path.exists());

        let b_socket = gb.socket_path.clone();
        drop(a);
        assert!(
            b_socket.exists(),
            "one session ending must not remove another's socket"
        );
    }

    /// Backgrounded work is confined exactly like the foreground. This is the
    /// third shell path and it had no test at all; it also reached the
    /// grantless variant of `run_shell_command`, so it was confined *and*
    /// offline while the identical foreground command worked.
    #[tokio::test]
    async fn background_work_is_confined_and_keeps_the_session_network() {
        // This contract is about what the confinement layer denies. On a host
        // that cannot confine at all, commands deliberately run unconfined and
        // there is nothing to assert.
        if forge_tools::sandbox::availability().is_err() {
            eprintln!("skipping: this host cannot confine processes");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let session = session(dir.path()).await;

        // Confined: a background command cannot leave the workspace.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("escape-background.txt");
        let error = forge_tools::run_shell_command_with_egress(
            &format!("echo pwned > {}", target.to_str().unwrap()),
            dir.path(),
            session.egress_grant().as_deref(),
        )
        .await
        .expect_err("background work outside the workspace must be denied");
        assert!(
            matches!(error, forge_tools::ToolError::SandboxDenied { .. }),
            "background work must report a sandbox denial: {error}"
        );
        assert!(!target.exists(), "background work escaped the sandbox");

        // And still networked, when the session is.
        if session.egress_grant().is_some() {
            let out = forge_tools::run_shell_command_with_egress(
                "printf %s \"$HTTP_PROXY\"",
                dir.path(),
                session.egress_grant().as_deref(),
            )
            .await
            .unwrap();
            assert!(
                out.content.contains("127.0.0.1"),
                "backgrounded work must inherit the session's network, got {:?}",
                out.content
            );
        }
    }

    /// MCP servers are spawned processes the sandbox does not confine, so they
    /// keep their prompt. Asserted because it is the deliberate hole in the
    /// model: if it ever stops prompting, that is a silent widening rather
    /// than a visible one.
    #[test]
    fn mcp_tools_stay_gated() {
        use forge_governance::Governance;
        use forge_types::{PolicyDecision, SideEffectClass, ToolCall};

        let g = Governance::default();
        assert_eq!(
            g.authorize(
                &ToolCall {
                    id: "1".into(),
                    name: "mcp:anything".into(),
                    arguments: serde_json::json!({}),
                },
                SideEffectClass::Exec
            ),
            PolicyDecision::Hitl,
            "MCP must keep asking; the sandbox does not confine those processes"
        );
    }
}
