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
    if let Some((broker, config)) = crate::sign_broker::start_for_repo(repo, scratch.path()).await {
        loopback_ports.push(broker.addr().port());
        crate::sign_broker::push_git_config(&mut env, config);
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
