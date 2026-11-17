//! `WorkspaceJournals`: the one way to get at a workspace's session journals
//! for reading — the report, the ranking that chooses models — so the rules
//! are in one place: the workspace's own journals only while the user
//! trusts it (an explicitly named directory is the user's own choice);
//! never a journal the repository tracks (someone committed it), nor a
//! symbolic link; and none when Git cannot tell which are tracked.

use std::path::{Path, PathBuf};

use forge_durable::{new_session_id, Journal};
use forge_session::workspace_journals::{UntrustedWorkspace, WorkspaceJournals};

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q", "--initial-branch=main"]);
    git(repo.path(), &["config", "user.email", "forge@example.com"]);
    git(repo.path(), &["config", "user.name", "Forge Test"]);
    git(
        repo.path(),
        &["commit", "-q", "--allow-empty", "-m", "init"],
    );
    repo
}

fn config(workspace: &Path) -> forge_config::Config {
    forge_config::Config {
        resolved_workspace: workspace.to_path_buf(),
        workspace_root: Some(workspace.display().to_string()),
        ..Default::default()
    }
}

/// A trust store trusting `workspaces`.
fn store(dir: &Path, workspaces: &[&Path]) -> PathBuf {
    let store = dir.join("trust.toml");
    for workspace in workspaces {
        forge_config::grant_trust_at(&store, workspace).unwrap();
    }
    store
}

async fn journal(dir: &Path) -> PathBuf {
    let id = new_session_id();
    drop(Journal::open(dir, id).await.unwrap());
    for _ in 0..500 {
        let busy = std::fs::read_dir(dir)
            .unwrap()
            .any(|e| e.unwrap().file_name().to_string_lossy().ends_with("-wal"));
        if !busy {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    dir.join(format!("{id}.db"))
}

#[test]
fn the_workspaces_own_journals_need_trust_and_a_named_directory_does_not() {
    let workspace = repo();
    let scratch = tempfile::tempdir().unwrap();
    let untrusting = store(scratch.path(), &[]);

    // The default location, and a path relative to the workspace: its own.
    let own = WorkspaceJournals::for_config(&config(workspace.path()), Some(untrusting.clone()));
    assert!(own.needs_trust());
    assert!(!own.is_trusted());
    assert_eq!(
        own.check_trust(),
        Err(UntrustedWorkspace {
            workspace: workspace.path().to_path_buf()
        })
    );
    let mut relative = config(workspace.path());
    relative.journal.path = "journals".into();
    assert!(WorkspaceJournals::for_config(&relative, Some(untrusting.clone())).needs_trust());

    // A directory named by an absolute path: the user's own choice.
    let mut named = config(workspace.path());
    named.journal.path = scratch.path().join("j").display().to_string();
    let named = WorkspaceJournals::for_config(&named, Some(untrusting));
    assert!(!named.needs_trust());
    assert!(named.is_trusted());
    assert_eq!(named.dir(), scratch.path().join("j"));
    assert!(WorkspaceJournals::at(scratch.path().to_path_buf()).is_trusted());
}

/// The directory is where the workspace's sessions keep their journals —
/// the same one the report reads.
#[test]
fn the_directory_is_the_reports() {
    let workspace = repo();
    let cfg = config(workspace.path());
    let journals = WorkspaceJournals::for_config(&cfg, None);
    assert_eq!(
        journals.dir(),
        forge_session::model_report::report_journal_dir(&cfg)
    );
    assert!(!workspace.path().join(".forge").exists(), "nothing created");
}

#[tokio::test]
async fn an_untrusted_workspaces_journals_cannot_be_listed() {
    let workspace = repo();
    let sessions = workspace.path().join(".forge/local/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    journal(&sessions).await;
    let scratch = tempfile::tempdir().unwrap();
    let journals =
        WorkspaceJournals::for_config(&config(workspace.path()), Some(store(scratch.path(), &[])));
    let error = journals.list().await.expect_err("refused");
    assert!(
        error.downcast_ref::<UntrustedWorkspace>().is_some(),
        "{error:#}"
    );
}

/// Every listing rule, in one place: committed journals, symbolic links and
/// files that are not session journals are not listed; the first two are
/// reported as skipped. (Read directly: a workspace's own `.forge/local` is
/// not read at all once Git tracks anything in it — `forge_storage`.)
#[tokio::test]
async fn a_listing_keeps_only_the_sessions_run_here() {
    let workspace = repo();
    let sessions = workspace.path().join(".forge/local/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let committed = journal(&sessions).await;
    git(workspace.path(), &["add", "-f", ".forge"]);
    git(workspace.path(), &["commit", "-q", "-m", "journals"]);
    let mine = journal(&sessions).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let target = journal(elsewhere.path()).await;
    let link = sessions.join(format!("{}.db", new_session_id()));
    std::os::unix::fs::symlink(&target, &link).unwrap();
    std::fs::write(sessions.join("notes.txt"), "x").unwrap();

    let listing = WorkspaceJournals::at(sessions.clone())
        .list()
        .await
        .unwrap();
    assert_eq!(listing.journals, [mine]);
    let mut skipped: Vec<(PathBuf, bool)> = listing
        .skipped
        .iter()
        .map(|s| (s.path.clone(), s.error.contains("committed")))
        .collect();
    skipped.sort();
    let mut expected = vec![(committed, true), (link, false)];
    expected.sort();
    assert_eq!(skipped, expected);
}

/// Inside a repository whose tracked files Git cannot list: nothing.
#[tokio::test]
async fn when_git_cannot_tell_nothing_is_listed() {
    let workspace = repo();
    let sessions = workspace.path().join(".forge/local/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    journal(&sessions).await;
    std::fs::write(workspace.path().join(".git/index"), "not an index").unwrap();
    let listing = WorkspaceJournals::at(sessions.clone())
        .list()
        .await
        .unwrap();
    assert!(listing.journals.is_empty());
    assert_eq!(listing.skipped.len(), 1);
}
