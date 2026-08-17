//! Deriving the permission mode from what the host can actually enforce.
//!
//! The invariant this file exists to hold:
//!
//! > Auto is only ever entered when there is an enforcement floor underneath
//! > it.
//!
//! `AcceptEdits` ("Auto") frees writes. That is only defensible when a sandbox
//! confines what a freed write can reach. On a host that cannot confine, the
//! honest mode is `Manual` — forge asks rather than running agent commands
//! unconfined and silent.
//!
//! This is a deliberate divergence from Claude Code, whose default is to warn
//! and continue *unsandboxed*, hardened only by opting into
//! `sandbox.failIfUnavailable`. That trades the floor for convenience at
//! exactly the moment the user cannot see it.
//!
//! This lives in `forge-core` because it is the lowest layer that can see both
//! halves: `forge-governance` owns [`PermissionMode`] but depends on
//! `forge-types` alone, and `forge-tools` knows about sandboxes but nothing
//! about approval. A future headless entry point gets the derivation for free
//! by living above this crate.

use forge_governance::PermissionMode;

/// The most permissive mode this host may use, and why it is capped when it is.
///
/// The result is a **ceiling, not an override**. Narrowing is always safe, so a
/// user who prefers `Manual` keeps it; widening past the ceiling is what must
/// not happen. Callers clamp with [`PermissionMode::clamped_to`].
pub fn permission_ceiling() -> (PermissionMode, Option<String>) {
    match forge_tools::sandbox::availability() {
        Ok(()) => (PermissionMode::AcceptEdits, None),
        Err(unavailable) => (PermissionMode::Manual, Some(unavailable.reason())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ceiling_matches_whether_this_host_can_confine() {
        let (ceiling, reason) = permission_ceiling();
        match forge_tools::sandbox::availability() {
            Ok(()) => {
                assert_eq!(ceiling, PermissionMode::AcceptEdits);
                assert!(reason.is_none(), "no cap means nothing to explain");
            }
            Err(_) => {
                assert_eq!(ceiling, PermissionMode::Manual);
                let reason = reason.expect("a capped mode must say why");
                assert!(!reason.is_empty());
            }
        }
    }

    /// The invariant, stated as a test: there is no combination of inputs that
    /// yields Auto without a sandbox.
    #[test]
    fn auto_is_unreachable_without_a_floor() {
        let capped = PermissionMode::AcceptEdits.clamped_to(PermissionMode::Manual);
        assert_eq!(capped, PermissionMode::Manual);
    }

    /// A user who wants to be asked more often is never overridden.
    #[test]
    fn narrowing_is_always_permitted() {
        assert_eq!(
            PermissionMode::Manual.clamped_to(PermissionMode::AcceptEdits),
            PermissionMode::Manual
        );
        assert_eq!(
            PermissionMode::Manual.clamped_to(PermissionMode::Manual),
            PermissionMode::Manual
        );
    }

    #[test]
    fn a_permitted_mode_passes_through_unchanged() {
        assert_eq!(
            PermissionMode::AcceptEdits.clamped_to(PermissionMode::AcceptEdits),
            PermissionMode::AcceptEdits
        );
    }
}

/// A running egress proxy plus the grant that points at it.
///
/// The proxy is kept alive by holding it: `EgressProxy` stops serving and
/// removes its socket on drop, so a caller that discards this leaves a grant
/// addressing nothing, and every network call inside the sandbox fails in a
/// way that looks like a broken network rather than a dropped proxy.
pub struct EgressRuntime {
    _proxy: forge_tools::egress::EgressProxy,
    grant: std::sync::Arc<forge_tools::sandbox::EgressGrant>,
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
}

/// Start the egress proxy for a session.
///
/// Returns `None` when a proxy cannot be started, which leaves the network off
/// — the safe direction. A session without egress still works; commands that
/// need the network fail with the sandbox's own explanation rather than
/// silently reaching it.
///
/// The allow-list is currently [`EgressPolicy::with_default_ecosystems`]: the
/// package registries a first build needs. That is a policy decision made on
/// the user's behalf, and it is the weakest part of this — Claude Code
/// pre-allows nothing and prompts per host, Codex defaults to `"*" = "deny"`.
/// Seeding is only defensible here because forge has no per-domain prompt yet;
/// when one exists this should become the fallback, not the default.
pub async fn start_egress(session_id: uuid::Uuid) -> Option<EgressRuntime> {
    use forge_tools::egress::{EgressPolicy, EgressProxy};

    let policy = EgressPolicy::with_default_ecosystems();
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
        }),
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
        let Some(runtime) = start_egress(uuid::Uuid::new_v4()).await else {
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
        let Some(runtime) = start_egress(uuid::Uuid::new_v4()).await else {
            return;
        };
        let path = runtime.grant().socket_path.clone();
        assert!(path.exists());
        drop(runtime);
        assert!(!path.exists(), "a stale socket would break the next bind");
    }

    /// The socket belongs outside the workspace: inside it the agent could
    /// delete or replace the thing carrying its own traffic.
    #[tokio::test]
    async fn the_socket_lives_outside_the_workspace() {
        let Some(runtime) = start_egress(uuid::Uuid::new_v4()).await else {
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

    /// Egress is on by default: a session starts its proxy and hands every
    /// tool a grant pointing at it. Without this the allow-list exists but
    /// nothing consults it, which is where this sat for most of its life.
    #[tokio::test]
    async fn a_session_starts_with_network_egress() {
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
