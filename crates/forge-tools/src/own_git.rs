//! The network for git that Forge itself runs (the Push, Pull and Fetch
//! buttons, pull-request publishing): the same gateway, host policy and SSH
//! broker the agent's commands in that workspace get.
//!
//! The crates that run this git sit below the gateway in the dependency
//! graph, so they ask `forge_sandbox::git` for a networked command and this
//! module answers through the provider hook. Which policy applies is decided
//! by where the repository is: a supervised session registers its workspace
//! with its egress grant, and git run on that workspace is held to it. A
//! host nobody has allowed is asked about in that session, like any other.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::egress::EgressInvocation;
use crate::sandbox::EgressGrant;

struct Entry {
    id: u64,
    workspace: PathBuf,
    grant: Arc<EgressGrant>,
}

static WORKSPACES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A workspace's registration; dropping it removes the entry.
#[derive(Debug)]
pub struct WorkspaceEgress {
    id: u64,
}

impl Drop for WorkspaceEgress {
    fn drop(&mut self) {
        entries().retain(|entry| entry.id != self.id);
    }
}

fn entries() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    WORKSPACES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Hold git that Forge runs on `workspace` to `grant`'s policy.
pub fn register_workspace_egress(workspace: &Path, grant: Arc<EgressGrant>) -> WorkspaceEgress {
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    entries().push(Entry {
        id,
        workspace,
        grant,
    });
    WorkspaceEgress { id }
}

/// The grant governing `repo`: that of the innermost registered workspace
/// containing it, the most recently registered when several are the same.
fn grant_for(repo: &Path) -> Option<Arc<EgressGrant>> {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    entries()
        .iter()
        .filter(|entry| repo.starts_with(&entry.workspace))
        .max_by_key(|entry| (entry.workspace.components().count(), entry.id))
        .map(|entry| Arc::clone(&entry.grant))
}

struct Provider {
    runtime: tokio::runtime::Handle,
}

impl forge_sandbox::git::NetworkProvider for Provider {
    fn open(&self, repo: &Path) -> Option<forge_sandbox::git::Network> {
        let grant = grant_for(repo)?;
        // The caller is synchronous code on some thread; the endpoints are
        // async. Start them on the runtime and wait here for the result.
        let (done, result) = std::sync::mpsc::channel();
        let repo = repo.to_path_buf();
        self.runtime.spawn(async move {
            let _ = done.send(open_network(&grant, &repo).await);
        });
        result
            .recv_timeout(std::time::Duration::from_secs(15))
            .ok()
            .flatten()
    }
}

async fn open_network(grant: &EgressGrant, repo: &Path) -> Option<forge_sandbox::git::Network> {
    let mut invocation = EgressInvocation::start_tcp_labeled(Some(grant), Some("forge-git"))
        .await
        .ok()??;
    let scratch = tempfile::Builder::new()
        .prefix("forge-git-")
        .tempdir()
        .ok()?;
    let mut env = crate::credentials::credential_helper_env(Some(grant), scratch.path(), repo);
    let mut loopback_ports = Vec::new();
    if let Some((broker, git_ssh_command)) =
        crate::shell_spawn::start_ssh_broker(&invocation, &[]).await
    {
        loopback_ports.push(broker.addr().port());
        env.push(("GIT_SSH_COMMAND".into(), git_ssh_command));
        env.push(("GIT_SSH_VARIANT".into(), "ssh".into()));
        invocation.hold(broker);
    }
    // Signing: the key stays on the host, and the broker signs with what
    // the user's own settings name.
    if let Some((broker, config)) = start_sign_broker(repo, scratch.path()).await {
        loopback_ports.push(broker.addr().port());
        push_git_config(&mut env, config);
        invocation.hold(broker);
    }
    Some(forge_sandbox::git::Network {
        proxy_port: invocation.tcp_addr().port(),
        loopback_ports,
        env,
        scratch: Some(scratch.path().to_path_buf()),
        keep: Box::new((invocation, scratch)),
    })
}

async fn start_sign_broker(
    repo: &Path,
    scratch: &Path,
) -> Option<(crate::sign_broker::SignBroker, Vec<(String, String)>)> {
    let exe = crate::git_network::helper_exe().ok()?;
    let repo = repo.to_path_buf();
    let resolve: crate::sign_broker::Resolve = Arc::new(move || {
        let repo = repo.clone();
        Box::pin(async move { crate::git_network::host_signer(&repo).await })
    });
    let broker = crate::sign_broker::SignBroker::start(resolve).await.ok()?;
    let config = broker.git_config(&exe, scratch).ok()?;
    Some((broker, config))
}

/// Add settings to the `GIT_CONFIG_COUNT`/`KEY`/`VALUE` variables in `env`,
/// after the ones already there.
fn push_git_config(env: &mut Vec<(String, String)>, config: Vec<(String, String)>) {
    let mut count = match env.iter().position(|(name, _)| name == "GIT_CONFIG_COUNT") {
        Some(at) => env.remove(at).1.parse().unwrap_or(0),
        None => 0usize,
    };
    for (key, value) in config {
        env.push((format!("GIT_CONFIG_KEY_{count}"), key));
        env.push((format!("GIT_CONFIG_VALUE_{count}"), value));
        count += 1;
    }
    env.push(("GIT_CONFIG_COUNT".into(), count.to_string()));
}

/// Give git that Forge runs its network through the gateway. Call once from
/// a multi-threaded runtime; a current-thread runtime is left alone, because
/// a synchronous caller waiting on it would be waiting on itself.
pub fn install_git_network(runtime: tokio::runtime::Handle) {
    if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
        forge_sandbox::git::set_network_provider(Arc::new(Provider { runtime }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> Arc<EgressGrant> {
        Arc::new(EgressGrant {
            proxy_port: 1,
            socket_path: PathBuf::new(),
            control: None,
        })
    }

    #[test]
    fn git_config_is_appended_after_the_settings_already_in_the_environment() {
        let mut env = vec![
            (
                "GIT_CONFIG_KEY_0".to_string(),
                "credential.helper".to_string(),
            ),
            ("GIT_CONFIG_VALUE_0".to_string(), String::new()),
            ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
        ];
        push_git_config(&mut env, vec![("gpg.program".into(), "/w".into())]);
        let value = |name: &str| {
            let found: Vec<_> = env.iter().filter(|(key, _)| key == name).collect();
            assert_eq!(found.len(), 1, "{name}");
            found[0].1.clone()
        };
        assert_eq!(value("GIT_CONFIG_COUNT"), "2");
        assert_eq!(value("GIT_CONFIG_KEY_0"), "credential.helper");
        assert_eq!(value("GIT_CONFIG_KEY_1"), "gpg.program");
        assert_eq!(value("GIT_CONFIG_VALUE_1"), "/w");
    }

    #[test]
    fn a_repository_gets_the_grant_of_the_innermost_workspace_containing_it() {
        let project = tempfile::tempdir().unwrap();
        let worktree = project.path().join(".forge/local/worktrees/s1");
        std::fs::create_dir_all(worktree.join("src")).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();

        let (outer, inner) = (grant(), grant());
        let outer_reg = register_workspace_egress(project.path(), Arc::clone(&outer));
        let inner_reg = register_workspace_egress(&worktree, Arc::clone(&inner));

        let found = |path: &Path| grant_for(path);
        assert!(Arc::ptr_eq(&found(project.path()).unwrap(), &outer));
        assert!(Arc::ptr_eq(&found(&worktree.join("src")).unwrap(), &inner));
        assert!(found(elsewhere.path()).is_none());

        drop(inner_reg);
        assert!(Arc::ptr_eq(&found(&worktree).unwrap(), &outer));
        drop(outer_reg);
        assert!(found(project.path()).is_none());
    }
}
