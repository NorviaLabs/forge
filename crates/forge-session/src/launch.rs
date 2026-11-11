//! Frontend-independent startup: a UI-free plan, then a launch.
//!
//! [`LaunchPlan::prepare`] runs before a frontend shows anything and writes
//! nothing. It probes repository ownership read-only, so a launch another
//! Forge would make a second writer is refused before any setup screen and
//! before any trust or theme write, and it reports whether the workspace
//! still needs trust. The frontend then runs its own screens (the desktop's
//! trust screen) and calls
//! [`LaunchPlan::launch`], which re-checks trust, then takes ownership,
//! recovers interrupted creations, resolves the session target and opens the
//! session.
//!
//! Nothing is written into the workspace before it is trusted: its content
//! (a symlinked `.forge`, a crafted `sessions.db`) is not ours to act on
//! until the user consents. The probe is advisory; `launch`'s real lease
//! acquisition still refuses a repository someone took in between.

use std::path::{Path, PathBuf};

use forge_config::Config;
use forge_types::SessionId;

use crate::{
    open_session, resolve_journal_dir, OpenedSession, RepositoryBootstrap, RepositorySessionError,
    RepositorySupervisorError, SessionLifecycle, SessionTarget,
};

/// Which session a launch should open, as the user asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchTarget {
    /// A fresh session.
    New,
    /// The workspace's last selected session (`--continue`). Falls back to
    /// the most recent session when nothing is selected or the selected
    /// session is gone.
    Continue,
    /// The most recent session by journal time (bare `--resume`).
    Latest,
    /// This session.
    Resume(SessionId),
    /// A new session forked from this one.
    Fork(SessionId),
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("workspace {} is not trusted; trust it before opening a session", .workspace.display())]
    Untrusted { workspace: PathBuf },
    #[error(transparent)]
    Repository(#[from] RepositorySupervisorError),
    #[error(
        "could not take ownership of this repository ({}): {source}. \
         Fix the permissions or contents of that directory (or move it aside), \
         then start Forge again; Forge will not run a second, unowned session beside it.",
        .control_dir.display()
    )]
    OwnershipUnavailable {
        control_dir: PathBuf,
        #[source]
        source: RepositorySupervisorError,
    },
    #[error(transparent)]
    Session(#[from] anyhow::Error),
}

/// Startup decisions taken before any screen is shown, without writing.
pub struct LaunchPlan {
    cfg: Config,
    workspace: PathBuf,
    needs_trust: bool,
    trust_store: Option<PathBuf>,
}

/// Everything a frontend needs to start running a session.
pub struct Launched {
    /// Project ownership. Every project, Git repository or plain directory,
    /// runs under its lease.
    pub bootstrap: RepositoryBootstrap,
    pub opened: OpenedSession,
    /// For a fork under a repository, the source session. The session in
    /// `opened` is the source itself; the caller forks it through the
    /// supervisor once it is running.
    pub fork_from: Option<SessionId>,
    /// Startup notices in display order: repository recovery, session
    /// assembly, then target resolution. `opened.notices` is left empty;
    /// its notices are moved here.
    pub notices: Vec<String>,
}

const NO_PREVIOUS_SESSION: &str = "No previous session found; creating a new session.";
const SELECTED_SESSION_GONE: &str =
    "The last selected session is no longer available; reopening the most recent session.";

impl LaunchPlan {
    /// Probe repository ownership for `cfg`'s workspace (read-only) and read
    /// its trust decision. Refuses when another process holds the lease.
    pub async fn prepare(cfg: &Config) -> Result<Self, LaunchError> {
        Self::prepare_with_trust_store(cfg, None).await
    }

    /// [`prepare`](Self::prepare) with the trust store redirected. Only a
    /// test has a reason to pass anything but `None`.
    pub async fn prepare_with_trust_store(
        cfg: &Config,
        trust_store: Option<PathBuf>,
    ) -> Result<Self, LaunchError> {
        let workspace = cfg.workspace_root().to_path_buf();
        probe_ownership(&workspace)?;
        let needs_trust = !is_trusted(trust_store.as_deref(), &workspace);
        Ok(Self {
            cfg: cfg.clone(),
            workspace,
            needs_trust,
            trust_store,
        })
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Whether the frontend must obtain trust for the workspace before
    /// [`launch`](Self::launch) will open a session.
    pub fn needs_trust(&self) -> bool {
        self.needs_trust
    }

    /// Take ownership, recover, resolve `target` and open the session. Trust
    /// is re-read first, so a frontend that skipped its trust screen is
    /// refused rather than trusted by default, and nothing is written into
    /// an untrusted workspace.
    pub async fn launch(self, target: LaunchTarget) -> Result<Launched, LaunchError> {
        if !is_trusted(self.trust_store.as_deref(), &self.workspace) {
            return Err(LaunchError::Untrusted {
                workspace: self.workspace,
            });
        }
        let cfg = self.cfg;
        // Repository ownership comes before any session exists. The
        // exclusive lease is what enforces one Forge per repository group;
        // acquiring it after `open_session` would let a losing process write
        // session state first and only then discover it does not own the
        // repository.
        let bootstrap = acquire_repository(&cfg, &self.workspace).await?;
        let mut notices = bootstrap.migration_notices().to_vec();
        match bootstrap.recover_interrupted_creations().await {
            Ok(recovered) => notices.extend(recovered),
            Err(error) => notices.push(format!("Session recovery incomplete: {error}")),
        }
        let (target, target_notices) = resolve_target(&cfg, &bootstrap, target).await?;

        let mut open_target = target;
        let mut fork_from = None;
        let session_cfg = match target {
            SessionTarget::Resume(session_id) => {
                session_config(&cfg, &bootstrap, session_id).await?
            }
            SessionTarget::Fork(session_id) => {
                let bound = session_config(&cfg, &bootstrap, session_id).await?;
                fork_from = Some(session_id);
                open_target = SessionTarget::Resume(session_id);
                bound
            }
            _ => cfg.clone(),
        };
        let mut opened = open_session(&session_cfg, open_target).await?;
        notices.append(&mut opened.notices);
        notices.extend(target_notices);
        Ok(Launched {
            bootstrap,
            opened,
            fork_from,
            notices,
        })
    }
}

/// `cfg` rebound to another workspace of the same repository, with that
/// workspace's journal directory.
pub fn config_for_workspace(cfg: &Config, workspace: &Path) -> Config {
    let mut bound = cfg.clone();
    bound.resolved_workspace = workspace.to_path_buf();
    bound.workspace_root = Some(workspace.display().to_string());
    let (journal_dir, _) = resolve_journal_dir(&bound);
    bound.journal.path = journal_dir.display().to_string();
    bound
}

/// The config for reopening `session_id`, bound to its recorded workspace.
///
/// A directory project's journals may predate the control database (they
/// were written by lease-less Forge versions and moved into `.forge/local`),
/// so they can have no row. Every session there runs in the project root, so
/// the launch config is already right; the supervisor registers the row when
/// it adopts the session.
async fn session_config(
    cfg: &Config,
    bootstrap: &RepositoryBootstrap,
    session_id: SessionId,
) -> Result<Config, LaunchError> {
    match bootstrap.session(session_id).await {
        Ok(record) => Ok(config_for_workspace(cfg, &record.workspace)),
        Err(RepositorySessionError::NotFound(_)) if !bootstrap.supports_git() => Ok(cfg.clone()),
        Err(error) => Err(LaunchError::Repository(RepositorySupervisorError::Control(
            error,
        ))),
    }
}

fn is_trusted(trust_store: Option<&Path>, workspace: &Path) -> bool {
    match trust_store {
        Some(store) => forge_config::is_trusted_at(store, workspace),
        None => forge_config::is_trusted(workspace),
    }
}

/// Read-only ownership check for [`LaunchPlan::prepare`]: refuse when
/// another process holds the repository lease. Creates and writes nothing,
/// so it is safe in an untrusted workspace.
fn probe_ownership(workspace: &Path) -> Result<(), LaunchError> {
    use forge_storage::RuntimeDataKind;
    let Ok(storage) = crate::ProjectTopology::detect(workspace).and_then(|t| t.storage()) else {
        return Ok(());
    };
    let Some(control_dir) = storage.path_for_read(RuntimeDataKind::Control) else {
        return Ok(());
    };
    match crate::RepositoryLease::probe(&control_dir) {
        Ok(()) => Ok(()),
        Err(error @ RepositorySessionError::AlreadyOwned { .. }) => {
            Err(LaunchError::Repository(error.into()))
        }
        // Anything else (an unreadable lock file) is decided by the real
        // acquisition after trust, which refuses it there.
        Err(_) => Ok(()),
    }
}

/// Take project ownership.
///
/// Any failure to take ownership is a refusal: another Forge owning it
/// (`AlreadyOwned`), an unreadable lock file or a control database error.
/// Falling back to a lease-less session could journal beside a live owner, a
/// second writer. Git repositories and plain directories alike run under the
/// lease.
async fn acquire_repository(
    cfg: &Config,
    workspace: &Path,
) -> Result<RepositoryBootstrap, LaunchError> {
    match RepositoryBootstrap::acquire(cfg).await {
        Ok(bootstrap) => Ok(bootstrap),
        Err(
            error @ RepositorySupervisorError::Control(RepositorySessionError::AlreadyOwned {
                ..
            }),
        ) => Err(LaunchError::Repository(error)),
        Err(source) => Err(LaunchError::OwnershipUnavailable {
            control_dir: crate::ProjectTopology::detect(workspace)
                .map(|topology| topology.root().to_path_buf())
                .unwrap_or_else(|_| workspace.to_path_buf())
                .join(".forge")
                .join("local"),
            source,
        }),
    }
}

/// Turn the requested target into a concrete one, plus any notice the
/// resolution produced.
async fn resolve_target(
    cfg: &Config,
    bootstrap: &RepositoryBootstrap,
    target: LaunchTarget,
) -> Result<(SessionTarget, Vec<String>), LaunchError> {
    match target {
        LaunchTarget::New => Ok((SessionTarget::New, Vec::new())),
        LaunchTarget::Resume(session_id) => Ok((SessionTarget::Resume(session_id), Vec::new())),
        LaunchTarget::Fork(session_id) => Ok((SessionTarget::Fork(session_id), Vec::new())),
        LaunchTarget::Latest => latest_target(cfg),
        LaunchTarget::Continue => {
            let selected = bootstrap
                .selected()
                .await
                .map_err(|error| LaunchError::Repository(error.into()))?;
            let Some(session_id) = selected else {
                return latest_target(cfg);
            };
            if selected_is_resumable(cfg, bootstrap, session_id).await {
                return Ok((SessionTarget::Resume(session_id), Vec::new()));
            }
            let (target, mut notices) = latest_target(cfg)?;
            notices.insert(0, SELECTED_SESSION_GONE.to_string());
            Ok((target, notices))
        }
    }
}

/// A selected session can be reopened only while it is active and its
/// journal still exists.
async fn selected_is_resumable(
    cfg: &Config,
    bootstrap: &RepositoryBootstrap,
    session_id: SessionId,
) -> bool {
    let Ok(record) = bootstrap.session(session_id).await else {
        return false;
    };
    if record.lifecycle != SessionLifecycle::Active {
        return false;
    }
    let (journal_dir, _) = resolve_journal_dir(&config_for_workspace(cfg, &record.workspace));
    journal_dir.join(format!("{session_id}.db")).is_file()
}

fn latest_target(cfg: &Config) -> Result<(SessionTarget, Vec<String>), LaunchError> {
    let (journal_dir, _) = resolve_journal_dir(cfg);
    let latest = most_recent_journal(&journal_dir)
        .map_err(|error| LaunchError::Session(anyhow::Error::new(error)))?;
    Ok(match latest {
        Some(session_id) => (SessionTarget::Resume(session_id), Vec::new()),
        None => (SessionTarget::New, vec![NO_PREVIOUS_SESSION.to_string()]),
    })
}

/// The session whose journal (`<id>.db`) was modified most recently.
fn most_recent_journal(dir: &Path) -> std::io::Result<Option<SessionId>> {
    if !dir.is_dir() {
        return Ok(None);
    }
    let mut latest: Option<(std::time::SystemTime, SessionId)> = None;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("db") {
            continue;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| SessionId::parse_str(value).ok())
        else {
            continue;
        };
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        if latest.is_none_or(|(time, _)| modified > time) {
            latest = Some((modified, id));
        }
    }
    Ok(latest.map(|(_, id)| id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NewRepositorySession, RepositoryControl, WorktreeOwnership};
    use forge_storage::{RuntimeDataKind, RuntimeStorage};
    use std::time::{Duration, SystemTime};
    use tempfile::TempDir;

    /// The desktop app launches from a Tauri command, whose future must be
    /// `Send`. It builds in its own workspace, so guard that here.
    #[allow(dead_code)]
    fn launch_futures_are_send(plan: LaunchPlan, cfg: &Config) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(plan.launch(LaunchTarget::Continue));
        assert_send(RepositoryBootstrap::acquire(cfg));
        assert_send(crate::RepositorySupervisor::open(cfg));
    }

    /// Dropped sessions close their journals asynchronously, and the close
    /// rewrites the `.db`. Wait (bounded) for the WAL and shared-memory
    /// files to go, so later modification times are not overwritten.
    async fn wait_closed(journal: &Path) {
        let wal = journal.with_extension("db-wal");
        let shm = journal.with_extension("db-shm");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while wal.exists() || shm.exists() {
            assert!(std::time::Instant::now() < deadline, "journal never closed");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    struct Fixture {
        repo: TempDir,
        trust: TempDir,
        cfg: Config,
    }

    impl Fixture {
        /// A git repository with one commit, a mock-model config journaling
        /// under the repository, and a private trust store.
        fn new(trusted: bool) -> Self {
            let repo = TempDir::new().unwrap();
            std::fs::write(repo.path().join("tracked.txt"), "one\n").unwrap();
            for args in [
                &["init", "-q", "-b", "main"][..],
                &["config", "user.email", "forge@example.test"][..],
                &["config", "user.name", "Forge Test"][..],
                &["add", "tracked.txt"][..],
                &["commit", "-q", "-m", "init"][..],
            ] {
                assert!(std::process::Command::new("git")
                    .arg("-C")
                    .arg(repo.path())
                    .args(args)
                    .status()
                    .unwrap()
                    .success());
            }
            let mut cfg = Config {
                resolved_workspace: repo.path().to_path_buf(),
                workspace_root: Some(repo.path().display().to_string()),
                ..Default::default()
            };
            cfg.journal.path = repo.path().join("journals").display().to_string();
            cfg.model.mock = true;
            let trust = TempDir::new().unwrap();
            if trusted {
                forge_config::grant_trust_at(&trust.path().join("trust.toml"), repo.path())
                    .unwrap();
            }
            Self { repo, trust, cfg }
        }

        async fn plan(&self) -> LaunchPlan {
            LaunchPlan::prepare_with_trust_store(
                &self.cfg,
                Some(self.trust.path().join("trust.toml")),
            )
            .await
            .unwrap()
        }

        fn journal(&self, session_id: SessionId) -> PathBuf {
            self.repo
                .path()
                .join("journals")
                .join(format!("{session_id}.db"))
        }

        async fn control(&self) -> RepositoryControl {
            let storage = crate::RepositoryRuntimeStorage::new(self.repo.path()).unwrap();
            RepositoryControl::open(&storage.path_for(RuntimeDataKind::Control).unwrap())
                .await
                .unwrap()
        }

        /// Create a session with a journal and register it on `workspace`.
        async fn session(&self, workspace: &Path, label: &str) -> SessionId {
            std::fs::create_dir_all(workspace).unwrap();
            let opened = open_session(
                &config_for_workspace(&self.cfg, workspace),
                SessionTarget::New,
            )
            .await
            .unwrap();
            let session_id = opened.session.session_id;
            drop(opened);
            assert!(self.journal(session_id).is_file(), "journal must exist");
            wait_closed(&self.journal(session_id)).await;
            self.control()
                .await
                .register_session(
                    NewRepositorySession {
                        session_id,
                        label: label.into(),
                        workspace: workspace.to_path_buf(),
                        branch: "main".into(),
                        ownership: if workspace == self.repo.path() {
                            WorktreeOwnership::Primary
                        } else {
                            WorktreeOwnership::Attached
                        },
                        slot: None,
                        model_id: "mock".into(),
                        profile_id: String::new(),
                        reasoning_effort: None,
                    },
                    None,
                )
                .await
                .unwrap();
            session_id
        }

        fn touch(&self, session_id: SessionId, age: Duration) {
            std::fs::File::options()
                .write(true)
                .open(self.journal(session_id))
                .unwrap()
                .set_modified(SystemTime::now() - age)
                .unwrap();
        }

        /// Two registered sessions: `older` (selected below) and `newer`,
        /// whose journal is the most recent.
        async fn two_sessions(&self) -> (SessionId, SessionId) {
            let older = self.session(self.repo.path(), "older").await;
            let newer = self.session(&self.repo.path().join("other"), "newer").await;
            self.touch(older, Duration::from_secs(3600));
            self.touch(newer, Duration::from_secs(0));
            (older, newer)
        }
    }

    #[tokio::test]
    async fn prepare_reports_whether_trust_is_needed() {
        let untrusted = Fixture::new(false);
        assert!(untrusted.plan().await.needs_trust());
        let trusted = Fixture::new(true);
        assert!(!trusted.plan().await.needs_trust());
    }

    #[tokio::test]
    async fn launch_refuses_an_untrusted_workspace_and_writes_nothing() {
        let fixture = Fixture::new(false);
        let before = tree(fixture.repo.path());
        let result = fixture.plan().await.launch(LaunchTarget::New).await;
        assert!(
            matches!(result, Err(LaunchError::Untrusted { .. })),
            "an untrusted workspace must be refused"
        );
        assert_eq!(
            changed(&before, &tree(fixture.repo.path())),
            Vec::<PathBuf>::new(),
            "nothing (no .forge, no exclude edit, no journal) before trust"
        );
    }

    #[tokio::test]
    async fn launch_refuses_a_repository_another_forge_took_after_prepare() {
        let fixture = Fixture::new(true);
        let plan = fixture.plan().await;
        let owner = RepositoryBootstrap::acquire(&fixture.cfg)
            .await
            .expect("another process takes the lease after the probe");
        let error = plan
            .launch(LaunchTarget::New)
            .await
            .err()
            .expect("the real acquisition must still refuse");
        assert!(
            error.to_string().contains("already owns this repository"),
            "{error}"
        );
        drop(owner);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn untrusted_symlinked_runtime_or_git_dirs_are_never_written_through() {
        for link in [
            ".forge",
            ".forge/local",
            ".forge/local/control",
            ".git/info",
        ] {
            let fixture = Fixture::new(false);
            let outside = TempDir::new().unwrap();
            let link_path = fixture.repo.path().join(link);
            if link_path.exists() {
                std::fs::remove_dir_all(&link_path).unwrap();
            }
            std::fs::create_dir_all(link_path.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(outside.path(), &link_path).unwrap();
            let before = tree(fixture.repo.path());

            let result = fixture.plan().await.launch(LaunchTarget::New).await;
            assert!(
                matches!(result, Err(LaunchError::Untrusted { .. })),
                "{link}: untrusted launch must be refused"
            );
            assert_eq!(
                changed(&before, &tree(fixture.repo.path())),
                Vec::<PathBuf>::new(),
                "{link}: nothing written inside the repository"
            );
            assert_eq!(
                std::fs::read_dir(outside.path()).unwrap().count(),
                0,
                "{link}: nothing written through the symlink"
            );
        }
    }

    #[tokio::test]
    async fn untrusted_crafted_control_database_is_not_opened() {
        let fixture = Fixture::new(false);
        let control = fixture.repo.path().join(".forge/local/control");
        std::fs::create_dir_all(&control).unwrap();
        std::fs::write(control.join("sessions.db"), b"not a database").unwrap();
        let before = tree(fixture.repo.path());

        let plan = fixture.plan().await;
        assert!(plan.needs_trust());
        let result = plan.launch(LaunchTarget::Continue).await;
        assert!(matches!(result, Err(LaunchError::Untrusted { .. })));
        assert_eq!(
            changed(&before, &tree(fixture.repo.path())),
            Vec::<PathBuf>::new(),
            "the crafted database is neither opened nor rewritten before trust"
        );
    }

    #[tokio::test]
    async fn continue_reopens_the_selected_session_not_the_most_recent() {
        let fixture = Fixture::new(true);
        let (older, newer) = fixture.two_sessions().await;
        fixture
            .control()
            .await
            .set_selected(Some(older))
            .await
            .unwrap();

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert_ne!(older, newer);
        assert_eq!(launched.opened.session.session_id, older);
        assert!(launched.bootstrap.supports_git());
        assert!(!launched
            .notices
            .iter()
            .any(|notice| notice == SELECTED_SESSION_GONE));
    }

    #[tokio::test]
    async fn continue_without_a_selection_reopens_the_most_recent() {
        let fixture = Fixture::new(true);
        let (_older, newer) = fixture.two_sessions().await;
        fixture.control().await.set_selected(None).await.unwrap();

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert_eq!(launched.opened.session.session_id, newer);
        assert!(!launched
            .notices
            .iter()
            .any(|notice| notice == SELECTED_SESSION_GONE));
    }

    #[tokio::test]
    async fn continue_falls_back_with_a_notice_when_the_selected_session_is_archived() {
        let fixture = Fixture::new(true);
        let (older, newer) = fixture.two_sessions().await;
        let control = fixture.control().await;
        control.set_selected(Some(older)).await.unwrap();
        control.archive(older).await.unwrap();
        drop(control);

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert_eq!(launched.opened.session.session_id, newer);
        assert!(launched
            .notices
            .iter()
            .any(|notice| notice == SELECTED_SESSION_GONE));
    }

    #[tokio::test]
    async fn continue_falls_back_with_a_notice_when_the_selected_journal_is_gone() {
        let fixture = Fixture::new(true);
        let (older, newer) = fixture.two_sessions().await;
        fixture
            .control()
            .await
            .set_selected(Some(older))
            .await
            .unwrap();
        std::fs::remove_file(fixture.journal(older)).unwrap();

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert_eq!(launched.opened.session.session_id, newer);
        assert!(launched
            .notices
            .iter()
            .any(|notice| notice == SELECTED_SESSION_GONE));
    }

    #[tokio::test]
    async fn latest_ignores_the_selection() {
        let fixture = Fixture::new(true);
        let (older, newer) = fixture.two_sessions().await;
        fixture
            .control()
            .await
            .set_selected(Some(older))
            .await
            .unwrap();

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Latest)
            .await
            .unwrap();
        assert_eq!(launched.opened.session.session_id, newer);
    }

    #[tokio::test]
    async fn continue_with_no_sessions_creates_one_and_says_so() {
        let fixture = Fixture::new(true);
        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert!(launched
            .notices
            .iter()
            .any(|notice| notice == NO_PREVIOUS_SESSION));
    }

    #[tokio::test]
    async fn prepare_on_an_owned_repository_is_refused() {
        let fixture = Fixture::new(true);
        let owner = RepositoryBootstrap::acquire(&fixture.cfg)
            .await
            .expect("first owner takes the lease");
        let result = LaunchPlan::prepare_with_trust_store(
            &fixture.cfg,
            Some(fixture.trust.path().join("trust.toml")),
        )
        .await;
        let error = result.err().expect("an owned repository must refuse");
        assert!(
            error.to_string().contains("already owns this repository"),
            "{error}"
        );
        assert!(!fixture.repo.path().join("journals").exists());
        drop(owner);
    }

    fn trust_store(fixture: &Fixture) -> PathBuf {
        fixture.trust.path().join("trust.toml")
    }

    /// Every file under `root` (bytes), skipping git's object store and
    /// other internals except `info/exclude`.
    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(next) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&next) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().to_path_buf();
                let in_git = relative.starts_with(".git");
                if path.is_dir() {
                    if !in_git
                        || relative == Path::new(".git")
                        || relative == Path::new(".git/info")
                    {
                        pending.push(path);
                    }
                } else if !in_git || relative == Path::new(".git/info/exclude") {
                    files.insert(relative, std::fs::read(&path).unwrap_or_default());
                }
            }
        }
        files
    }

    fn changed(
        before: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
        after: &std::collections::BTreeMap<PathBuf, Vec<u8>>,
    ) -> Vec<PathBuf> {
        before
            .keys()
            .chain(after.keys())
            .filter(|path| before.get(*path) != after.get(*path))
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// The user declines (or cancels) the trust screen: the plan is dropped
    /// without a launch, and the repository is exactly as it was.
    #[tokio::test]
    async fn declined_trust_writes_nothing() {
        let fixture = Fixture::new(false);
        let before = tree(fixture.repo.path());
        let plan = fixture.plan().await;
        assert!(plan.needs_trust());
        drop(plan);
        assert_eq!(
            changed(&before, &tree(fixture.repo.path())),
            Vec::<PathBuf>::new()
        );
        assert!(!fixture.repo.path().join(".forge").exists());
    }

    #[tokio::test]
    async fn prepare_writes_nothing() {
        for trusted in [false, true] {
            let fixture = Fixture::new(trusted);
            let store = trust_store(&fixture);
            let store_before = std::fs::read(&store).ok();
            let before = tree(fixture.repo.path());
            let plan = fixture.plan().await;
            let after = tree(fixture.repo.path());
            assert_eq!(
                changed(&before, &after),
                Vec::<PathBuf>::new(),
                "prepare (trusted={trusted}) must not write"
            );
            assert_eq!(
                std::fs::read(&store).ok(),
                store_before,
                "trust store touched"
            );
            drop(plan);
        }
    }

    #[tokio::test]
    async fn trust_granted_between_prepare_and_launch_is_honoured() {
        let fixture = Fixture::new(false);
        let plan = fixture.plan().await;
        assert!(plan.needs_trust());
        // What accepting the trust screen does.
        forge_config::grant_trust_at(&trust_store(&fixture), fixture.repo.path()).unwrap();
        plan.launch(LaunchTarget::New)
            .await
            .expect("trust granted after prepare must be honoured");
    }

    /// Discriminates a re-read from a cached `needs_trust`: prepare saw a
    /// trusted workspace, so only reading trust again at launch refuses.
    #[tokio::test]
    async fn trust_revoked_between_prepare_and_launch_is_refused_and_opens_nothing() {
        let fixture = Fixture::new(true);
        let plan = fixture.plan().await;
        assert!(!plan.needs_trust());
        std::fs::remove_file(trust_store(&fixture)).unwrap();
        let before = tree(fixture.repo.path());

        let result = plan.launch(LaunchTarget::New).await;
        assert!(matches!(result, Err(LaunchError::Untrusted { .. })));
        assert_eq!(
            changed(&before, &tree(fixture.repo.path())),
            Vec::<PathBuf>::new(),
            "a refused launch writes nothing"
        );
        assert!(
            !trust_store(&fixture).exists(),
            "the trust store stays revoked"
        );
        // No session was opened, so no journal exists and no model request
        // could have been made.
        assert!(!fixture.repo.path().join("journals").exists());
    }

    #[tokio::test]
    async fn continue_reopens_a_selected_worktree_session_in_its_own_workspace() {
        let fixture = Fixture::new(true);
        let root = fixture.session(fixture.repo.path(), "root").await;
        let other = fixture.repo.path().join("other");
        let worktree = fixture.session(&other, "worktree").await;
        fixture.touch(worktree, Duration::from_secs(3600));
        fixture.touch(root, Duration::from_secs(0));
        fixture
            .control()
            .await
            .set_selected(Some(worktree))
            .await
            .unwrap();

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert_eq!(launched.opened.session.session_id, worktree);
        assert_eq!(launched.opened.session.workspace_root(), other.as_path());
    }

    #[tokio::test]
    async fn continue_falls_back_when_the_selected_session_was_an_interrupted_creation() {
        let fixture = Fixture::new(true);
        let (_older, newer) = fixture.two_sessions().await;
        let control = fixture.control().await;
        let creation = control
            .begin_managed_creation("interrupted", fixture.repo.path(), None)
            .await
            .unwrap();
        let workspace = fixture.repo.path().join("interrupted");
        std::fs::create_dir_all(&workspace).unwrap();
        let interrupted = open_session(
            &config_for_workspace(&fixture.cfg, &workspace),
            SessionTarget::New,
        )
        .await
        .unwrap()
        .session
        .session_id;
        wait_closed(&fixture.journal(interrupted)).await;
        control
            .register_session(
                NewRepositorySession {
                    session_id: interrupted,
                    label: "interrupted".into(),
                    workspace: workspace.clone(),
                    branch: "main".into(),
                    ownership: WorktreeOwnership::Managed,
                    slot: None,
                    model_id: "mock".into(),
                    profile_id: String::new(),
                    reasoning_effort: None,
                },
                Some(creation.operation_id),
            )
            .await
            .unwrap();
        control.set_selected(Some(interrupted)).await.unwrap();
        fixture.touch(interrupted, Duration::from_secs(7200));
        drop(control);

        let launched = fixture
            .plan()
            .await
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        assert_eq!(launched.opened.session.session_id, newer);
        assert!(launched
            .notices
            .iter()
            .any(|notice| notice.contains("rolled back interrupted task")));
        assert!(launched
            .notices
            .iter()
            .any(|notice| notice == SELECTED_SESSION_GONE));
    }

    #[tokio::test]
    async fn continue_outside_git_reopens_the_most_recent_without_a_notice() {
        let dir = TempDir::new().unwrap();
        let mut cfg = Config {
            resolved_workspace: dir.path().to_path_buf(),
            workspace_root: Some(dir.path().display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = dir.path().join("journals").display().to_string();
        cfg.model.mock = true;
        let first = open_session(&cfg, SessionTarget::New)
            .await
            .unwrap()
            .session
            .session_id;
        let second = open_session(&cfg, SessionTarget::New)
            .await
            .unwrap()
            .session
            .session_id;
        for id in [first, second] {
            wait_closed(&dir.path().join("journals").join(format!("{id}.db"))).await;
        }
        for (id, age) in [(first, 0), (second, 3600)] {
            std::fs::File::options()
                .write(true)
                .open(dir.path().join("journals").join(format!("{id}.db")))
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
        let trust = TempDir::new().unwrap();
        let store = trust.path().join("trust.toml");
        forge_config::grant_trust_at(&store, dir.path()).unwrap();

        let launched = LaunchPlan::prepare_with_trust_store(&cfg, Some(store))
            .await
            .unwrap()
            .launch(LaunchTarget::Continue)
            .await
            .unwrap();
        // A plain folder runs under the lease, and its journals resume even
        // though they predate the control database.
        assert!(!launched.bootstrap.supports_git());
        assert_eq!(launched.opened.session.session_id, first);
        assert!(!launched
            .notices
            .iter()
            .any(|notice| notice == SELECTED_SESSION_GONE || notice == NO_PREVIOUS_SESSION));
    }

    #[tokio::test]
    async fn non_git_workspace_launches_under_a_lease() {
        let dir = TempDir::new().unwrap();
        let mut cfg = Config {
            resolved_workspace: dir.path().to_path_buf(),
            workspace_root: Some(dir.path().display().to_string()),
            ..Default::default()
        };
        cfg.journal.path = dir.path().join("journals").display().to_string();
        cfg.model.mock = true;
        let trust = TempDir::new().unwrap();
        let store = trust.path().join("trust.toml");
        forge_config::grant_trust_at(&store, dir.path()).unwrap();

        let launched = LaunchPlan::prepare_with_trust_store(&cfg, Some(store))
            .await
            .unwrap()
            .launch(LaunchTarget::New)
            .await
            .unwrap();
        assert!(!launched.bootstrap.supports_git());
        assert!(dir.path().join(".forge/local/control").is_dir());

        // While this process owns the folder, a second launch is refused
        // before it writes anything.
        assert!(matches!(
            LaunchPlan::prepare(&cfg).await,
            Err(LaunchError::Repository(RepositorySupervisorError::Control(
                RepositorySessionError::AlreadyOwned { .. }
            )))
        ));
        drop(launched);
    }
}
