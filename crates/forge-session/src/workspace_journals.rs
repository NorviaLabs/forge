//! The one way to read a workspace's session journals outside a session —
//! the model report (`model_report`), the ranking that chooses models
//! (`subagent_models`, `auto_models`) — so the rules that keep someone
//! else's journals out are in one place:
//!
//! - **Trust.** Forge runs no session in a workspace the user has not
//!   trusted, so journals found in one are not the user's (they could have
//!   been committed to the repository): the workspace's own journals are
//!   read only while the user trusts it, checked at every listing. A
//!   directory the user named explicitly needs no trust.
//! - **Committed journals.** `.forge/local` is only git-excluded, so a
//!   repository can commit files there: a journal Git tracks is not a
//!   session run here, and is skipped. When Git cannot tell which files it
//!   tracks, none is read.
//! - **Symbolic links.** Forge writes journals as files; a link could pull
//!   in another project's journal, or any file, and is skipped.
//!
//! The directory is found without creating anything.

use std::path::{Path, PathBuf};

use anyhow::Context;
use forge_config::Config;
use serde::Serialize;

/// A journal that was not read, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SkippedJournal {
    pub path: PathBuf,
    pub error: String,
}

/// The workspace is not trusted, and the journals asked for are the
/// workspace's own: Forge wrote none of them, so they are not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{}` is not trusted: Forge runs no sessions in a workspace you have not trusted, \
     so any journals there are not yours",
    workspace.display()
)]
pub struct UntrustedWorkspace {
    pub workspace: PathBuf,
}

/// Whose permission reading the journals takes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TrustGate {
    /// A directory the user named: theirs to read.
    Named,
    /// The workspace's own journals: read while the user trusts it
    /// (`store`: the trust store, `None` for the user's own).
    Workspace {
        workspace: PathBuf,
        store: Option<PathBuf>,
    },
}

/// The session journals that may be read, in path order, and those that
/// may not.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JournalListing {
    pub journals: Vec<PathBuf>,
    pub skipped: Vec<SkippedJournal>,
}

/// A workspace's journal directory, and the permission reading it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceJournals {
    dir: PathBuf,
    gate: TrustGate,
}

/// Whether the user trusts `workspace` (`store`: the trust store, `None`
/// for the user's own).
pub(crate) fn is_workspace_trusted(store: Option<&Path>, workspace: &Path) -> bool {
    crate::launch::is_trusted(store, workspace)
}

impl WorkspaceJournals {
    /// The journals `cfg` points at, as the report reads them: a directory
    /// the configuration names by an absolute path (`--journal`) is the
    /// user's own choice; otherwise — the default location, or a path
    /// relative to the workspace — they are the workspace's own.
    pub fn for_config(cfg: &Config, trust_store: Option<PathBuf>) -> Self {
        let dir = crate::journal_dir(cfg, crate::JournalDirFor::Reading).0;
        let gate = if Path::new(&cfg.journal.path).is_absolute() {
            TrustGate::Named
        } else {
            TrustGate::Workspace {
                workspace: cfg.workspace_root().to_path_buf(),
                store: trust_store,
            }
        };
        Self { dir, gate }
    }

    /// `workspace`'s own journals in `dir`, read only while the user
    /// trusts it — what choosing models reads, whatever the path.
    pub fn for_workspace(dir: PathBuf, workspace: PathBuf, trust_store: Option<PathBuf>) -> Self {
        Self {
            dir,
            gate: TrustGate::Workspace {
                workspace,
                store: trust_store,
            },
        }
    }

    /// A directory the user named: no trust needed.
    pub fn at(dir: PathBuf) -> Self {
        Self {
            dir,
            gate: TrustGate::Named,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether reading takes the user's trust of the workspace.
    pub fn needs_trust(&self) -> bool {
        matches!(self.gate, TrustGate::Workspace { .. })
    }

    /// Whether the journals may be read now.
    pub fn is_trusted(&self) -> bool {
        self.check_trust().is_ok()
    }

    /// `Err` when the workspace's own journals are asked for and the user
    /// does not trust it.
    pub fn check_trust(&self) -> Result<(), UntrustedWorkspace> {
        match &self.gate {
            TrustGate::Named => Ok(()),
            TrustGate::Workspace { workspace, store } => {
                if is_workspace_trusted(store.as_deref(), workspace) {
                    Ok(())
                } else {
                    Err(UntrustedWorkspace {
                        workspace: workspace.clone(),
                    })
                }
            }
        }
    }

    /// The session journals that may be read, and those skipped (see the
    /// module's rules). Refused with [`UntrustedWorkspace`] — trust checked
    /// now — before anything is looked at. A missing directory lists
    /// nothing (and is not created).
    pub async fn list(&self) -> anyhow::Result<JournalListing> {
        self.check_trust()?;
        let mut listing = JournalListing::default();
        if !self.dir.is_dir() {
            return Ok(listing);
        }
        let tracked = {
            let dir = self.dir.clone();
            tokio::task::spawn_blocking(move || forge_storage::tracked_files(&dir)).await?
        };
        for entry in std::fs::read_dir(&self.dir)
            .with_context(|| format!("could not list {}", self.dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !is_session_journal_name(&path) {
                continue;
            }
            // The entry's own type: a symbolic link is not followed.
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                listing.skipped.push(SkippedJournal {
                    path,
                    error: "a symbolic link, not a journal file: not followed".into(),
                });
            } else if file_type.is_file() {
                match &tracked {
                    Ok(None) => listing.journals.push(path),
                    Ok(Some(names)) if !names.contains(&entry.file_name()) => {
                        listing.journals.push(path)
                    }
                    Ok(Some(_)) => listing.skipped.push(SkippedJournal {
                        path,
                        error: "committed to the repository, not a session run here: not read"
                            .into(),
                    }),
                    Err(error) => listing.skipped.push(SkippedJournal {
                        path,
                        error: format!(
                            "cannot tell whether it was committed to the repository ({error}): \
                             not read"
                        ),
                    }),
                }
            }
        }
        listing.journals.sort();
        listing.skipped.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(listing)
    }
}

/// Whether `path` is named like a session journal: `<session uuid>.db`.
fn is_session_journal_name(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "db")
        && path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.parse::<uuid::Uuid>().is_ok())
}
