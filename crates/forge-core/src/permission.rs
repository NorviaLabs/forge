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
    /// `None` when the session's own listener could not start. The policy
    /// and the grant still exist, so every tool is still held to the policy;
    /// see [`start_egress_with_shared`].
    _proxy: Option<forge_tools::egress::EgressProxy>,
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

    /// Replace the host policy in place: same listener, socket and event
    /// log, and every endpoint sharing this state (shell invocations, MCP
    /// servers) enforces the new policy from its next CONNECT.
    pub fn replace_policy(&self, policy: forge_tools::egress::EgressPolicy) {
        self.shared.replace_policy(policy);
    }

    /// The live state this runtime serves.
    pub fn shared(&self) -> &forge_tools::egress::EgressShared {
        &self.shared
    }

    /// The gateway's loopback TCP listener (`127.0.0.1`, ephemeral port), for
    /// in-process and host-side callers that route through the gateway.
    /// `None` when the session's listener could not start.
    pub fn tcp_addr(&self) -> Option<std::net::SocketAddr> {
        self._proxy.as_ref().map(|proxy| proxy.tcp_addr())
    }

    /// Take every connection event recorded for this session, oldest first,
    /// including those made through per-invocation endpoints.
    pub fn drain_events(&self) -> Vec<forge_tools::egress::EgressEvent> {
        self.shared.drain_events()
    }
}

/// Hosts this workspace may reach: the built-in allow-list (code hosts and
/// package registries) plus the merged permission files.
///
/// Beyond the built-in list a host is reachable only when the personal
/// `permissions.toml` or the project's grants contain a matching
/// `host(...)` allow (or `host(*)` for unrestricted network); anything else
/// is asked about. A personal `deny` wins over the built-in list.
/// Repo-committed `allow` rules never loosen this — same trust split as the
/// HITL path.
pub fn egress_policy_for_workspace(
    workspace: &std::path::Path,
) -> forge_tools::egress::EgressPolicy {
    let (permissions, _) = forge_config::load_permissions(workspace);
    forge_tools::egress::EgressPolicy::from_permissions(&permissions).with_builtin_hosts()
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
    start_egress_with_shared(
        session_id,
        forge_tools::egress::EgressShared::new(policy, Default::default()),
    )
    .await
}

/// As [`start_egress`], serving an existing live state — one that other
/// endpoints (MCP servers started before the session) already share.
pub async fn start_egress_with_shared(
    session_id: uuid::Uuid,
    shared: forge_tools::egress::EgressShared,
) -> Option<EgressRuntime> {
    use forge_tools::egress::EgressProxy;

    // The socket lives outside the workspace: inside it, the agent could
    // delete or replace the thing carrying its own traffic, and `.forge` is
    // read-only in the sandbox anyway. Only Linux routes through it (its
    // network namespace has no loopback route to the host); on macOS the
    // sandbox reaches the loopback port, and binding a socket there only
    // added a way to fail: the per-user temp path can exceed the 104-byte
    // limit on a socket address.
    let socket_path = std::env::temp_dir().join(format!("forge-egress-{session_id}.sock"));
    let proxy = match EgressProxy::start_with_shared(shared.clone()).await {
        Ok(mut proxy) => {
            let socket = if cfg!(target_os = "linux") {
                proxy
                    .serve_on_unix_socket(&socket_path, forge_tools::egress::EgressPolicy::new())
                    .await
            } else {
                Ok(())
            };
            match socket {
                Ok(()) => Some(proxy),
                Err(error) => {
                    tracing::warn!(%error, "egress socket could not be bound");
                    None
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "egress proxy could not start");
            None
        }
    };

    // A session whose listener failed still gets a grant carrying the live
    // policy. Without one, `web_fetch` and the `git` tool read "no grant" as
    // "no policy" and reached any host directly. With it they check the
    // policy as usual, and each call's own endpoint either starts or fails
    // the call: nothing falls back to an unfiltered connection.
    Some(runtime_for(shared, proxy, socket_path))
}

fn runtime_for(
    shared: forge_tools::egress::EgressShared,
    proxy: Option<forge_tools::egress::EgressProxy>,
    socket_path: std::path::PathBuf,
) -> EgressRuntime {
    EgressRuntime {
        grant: std::sync::Arc::new(forge_tools::sandbox::EgressGrant {
            proxy_port: proxy.as_ref().map_or(0, |proxy| proxy.addr().port()),
            socket_path,
            control: Some(shared.clone()),
        }),
        shared,
        _proxy: proxy,
    }
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
        if cfg!(target_os = "linux") {
            assert!(
                grant.socket_path.exists(),
                "the socket must exist while the session holds the proxy"
            );
        }
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", grant.proxy_port))
                .await
                .is_ok(),
            "the proxy must accept connections"
        );
    }

    /// A session whose listener could not start is still held to its policy:
    /// the grant carries the live policy, so `web_fetch` and the `git` tool
    /// refuse a host that was not allowed instead of treating "no proxy" as
    /// "no policy", and a host granted later is honoured.
    #[tokio::test]
    async fn a_session_without_a_listener_still_enforces_its_policy() {
        let shared = forge_tools::egress::EgressShared::new(
            forge_tools::egress::EgressPolicy::new(),
            Default::default(),
        );
        let runtime = runtime_for(shared, None, std::path::PathBuf::from("/nonexistent"));
        let grant = runtime.grant();
        assert!(
            grant.control.is_some(),
            "the policy must travel with the grant"
        );
        assert!(!grant.permits_host("example.com"));
        runtime.grant_host("example.com");
        assert!(grant.permits_host("example.com"));
        assert!(runtime.tcp_addr().is_none());

        // A call's own endpoint is started from the grant, not from the
        // session listener, so tools keep working through the policy.
        let gateway = forge_tools::egress::CallGateway::start(Some(&grant), Some("call-1"))
            .await
            .expect("a per-call endpoint starts without the session listener");
        assert!(gateway.is_some());
    }

    /// The gateway's loopback TCP listener is exposed on the runtime and its
    /// decisions land in the session event log.
    #[tokio::test]
    async fn the_runtime_exposes_the_tcp_listener_and_its_events() {
        let Some(runtime) = start_egress(
            uuid::Uuid::new_v4(),
            forge_tools::egress::EgressPolicy::new(),
        )
        .await
        else {
            return;
        };
        let addr = runtime.tcp_addr().expect("the listener started");
        assert!(addr.ip().is_loopback(), "{addr}");
        assert_eq!(addr.port(), runtime.grant().proxy_port);
        let status = connect_status(addr.port(), "crates.io:443").await;
        assert!(status.contains("403"), "{status}");
        let mut events = Vec::new();
        for _ in 0..50 {
            events.extend(runtime.drain_events());
            if !events.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].host, "crates.io");
        assert!(!events[0].allowed());
        assert!(
            runtime.drain_events().is_empty(),
            "draining empties the log"
        );
    }

    /// Dropping the session must not leave a socket behind for the next one to
    /// collide with. Only Linux binds one.
    #[tokio::test]
    #[cfg(target_os = "linux")]
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
pub(crate) mod test_env {
    /// Serializes tests that redirect `HOME` / `XDG_CONFIG_HOME` so they
    /// cannot race with each other. Restores the environment on drop.
    pub(crate) struct IsolatedUserConfig {
        _lock: std::sync::MutexGuard<'static, ()>,
        _home: std::path::PathBuf,
        saved: Vec<(String, Option<String>)>,
    }

    impl IsolatedUserConfig {
        pub(crate) fn new() -> Self {
            static ENV_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
            let _lock = ENV_LOCK
                .get_or_init(|| std::sync::Mutex::new(()))
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::new();
            for key in ["HOME", "XDG_CONFIG_HOME"] {
                saved.push((key.to_string(), std::env::var(key).ok()));
            }
            // Kept for the life of the process: tests running at the same
            // time read `HOME` too, and must never find it deleted.
            let home = tempfile::TempDir::new().unwrap().keep();
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::set_var("HOME", &home);
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
}

#[cfg(test)]
mod session_egress_tests {
    use super::start_egress;
    use crate::*;

    use super::test_env::IsolatedUserConfig;

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

    /// A session in an empty workspace, with no personal host rules, starts
    /// with the built-in list (package registries and code hosts) and
    /// nothing else. A personal deny takes a built-in host away.
    #[tokio::test]
    async fn a_session_starts_with_the_builtin_hosts_and_nothing_else() {
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
            assert!(policy.permits(host), "{host} is built in");
        }
        for host in ["example.com", "pastebin.com", "evil.github.io"] {
            assert!(!policy.permits(host), "{host} must be asked about");
        }

        let user_path = forge_config::user_permissions_path().expect("redirected HOME");
        std::fs::create_dir_all(user_path.parent().unwrap()).unwrap();
        std::fs::write(&user_path, "deny = [\"host(**.github.com)\"]\n").unwrap();
        let policy = super::egress_policy_for_workspace(dir.path());
        assert!(!policy.permits("api.github.com") && policy.permits("crates.io"));
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
            !policy.permits("example.org"),
            "a repo-committed host allow must not loosen egress"
        );

        let user_path = forge_config::user_permissions_path().expect("redirected HOME");
        std::fs::create_dir_all(user_path.parent().unwrap()).unwrap();
        std::fs::write(&user_path, "allow = [\"host(**.example.com)\"]\n").unwrap();
        let policy = super::egress_policy_for_workspace(dir.path());
        assert!(policy.permits("api.example.com"));
        assert!(policy.permits("example.com"));
        assert!(!policy.permits("example.org"));
    }

    /// Applying a personal host policy must replace the empty default, not
    /// leave the original deny-all proxy in place.
    #[tokio::test]
    async fn applying_a_host_allow_replaces_the_session_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let model = std::sync::Arc::new(forge_model::MockModelClient::script(vec![]));
        let mut session = AgentSession::create(
            LoopConfig {
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

    /// A session that adopts egress state built before it (the state MCP
    /// servers route through) serves that state: session grants and policy
    /// replacements land on it, and its event log is the session's.
    #[tokio::test]
    async fn an_adopted_egress_state_receives_grants_and_policy_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let model = std::sync::Arc::new(forge_model::MockModelClient::script(vec![]));
        let mut session = AgentSession::create(
            LoopConfig {
                workspace: dir.path().to_path_buf(),
                journal_dir: dir.path().join("j"),
                ..Default::default()
            },
            model,
            forge_tools::ToolRegistry::new(),
        )
        .await
        .unwrap();
        let shared = forge_tools::egress::EgressShared::new(
            forge_tools::egress::EgressPolicy::new(),
            Default::default(),
        );
        session.adopt_egress_state(shared.clone()).await;
        let Some(grant) = session.egress_grant() else {
            return;
        };

        session.grant_egress_host("granted.example");
        assert!(shared.permits_host("granted.example"));

        let mut policy = forge_tools::egress::EgressPolicy::new();
        policy.allow("applied.example");
        session.apply_egress_policy(policy).await;
        assert!(shared.permits_host("applied.example"));
        assert!(!shared.permits_host("granted.example"));
        assert_eq!(
            session.egress_grant().unwrap().proxy_port,
            grant.proxy_port,
            "applying a policy must not replace the gateway"
        );

        let status = connect_status(grant.proxy_port, "refused.example:443").await;
        assert!(status.contains("403"), "{status:?}");
        assert!(
            shared
                .drain_events()
                .iter()
                .any(|event| event.host == "refused.example"),
            "the session gateway must record into the adopted state"
        );
    }

    /// Egress is on by default: a session starts its proxy and hands every
    /// tool a grant pointing at it. Without this the allow-list exists but
    /// nothing consults it, which is where this sat for most of its life.
    #[tokio::test]
    async fn a_session_starts_with_network_egress() {
        let Some(runtime) = start_egress(
            uuid::Uuid::new_v4(),
            forge_tools::egress::EgressPolicy::new(),
        )
        .await
        else {
            eprintln!("skipping: this host cannot start the egress proxy");
            return;
        };
        assert!(runtime.grant().proxy_port > 0);
        assert!(cfg!(not(target_os = "linux")) || runtime.grant().socket_path.exists());
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
                    mode: forge_tools::AgentMode::Writer,
                    tool_allowlist: None,
                    profile: Default::default(),
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
            !child.egress.is_some(),
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
        drop(a);
        assert!(
            tokio::net::TcpStream::connect(("127.0.0.1", gb.proxy_port))
                .await
                .is_ok(),
            "one session ending must not stop another's proxy"
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
