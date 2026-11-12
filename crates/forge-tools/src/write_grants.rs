//! Directories outside the workspace that sandboxed commands may write,
//! because the user said so (D8: widen the sandbox by one named thing).
//!
//! Granted for a session (held by that session's [`SessionWriteGrants`]) or
//! for the project (stored outside the workspace by `forge-config`). Every
//! shell spawn on the workspace picks them up; each was checked with
//! [`forge_sandbox::grantable_write_dir`] when it was granted.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

struct Entry {
    id: u64,
    workspace: PathBuf,
    dirs: Arc<Mutex<Vec<PathBuf>>>,
}

static SESSIONS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn sessions() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    SESSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// One session's write grants for its workspace. Dropping it ends them.
#[derive(Debug)]
pub struct SessionWriteGrants {
    id: u64,
    dirs: Arc<Mutex<Vec<PathBuf>>>,
}

impl SessionWriteGrants {
    pub fn register(workspace: &Path) -> Self {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dirs = Arc::new(Mutex::new(Vec::new()));
        sessions().push(Entry {
            id,
            workspace: canonical(workspace),
            dirs: Arc::clone(&dirs),
        });
        Self { id, dirs }
    }

    /// Let this session's commands write `dir` until the session ends.
    pub fn grant(&self, dir: PathBuf) {
        let mut dirs = self
            .dirs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
}

impl Drop for SessionWriteGrants {
    fn drop(&mut self) {
        sessions().retain(|entry| entry.id != self.id);
    }
}

/// Every directory sandboxed commands on `workspace` may write besides the
/// workspace itself: the sessions' grants and the project's.
pub fn write_grants_for(workspace: &Path) -> Vec<PathBuf> {
    let root = canonical(workspace);
    let mut dirs: Vec<PathBuf> = sessions()
        .iter()
        .filter(|entry| entry.workspace == root)
        .flat_map(|entry| {
            entry
                .dirs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
        .collect();
    for dir in forge_config::project_write_dirs(workspace) {
        // Checked again: the rules may have changed since it was stored.
        if let Ok(dir) = forge_sandbox::grantable_write_dir(&dir) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// The directory a denied path asks for: the path itself when it is a
/// directory, otherwise the directory it is in. `Err` when that directory
/// may not be granted.
pub fn grant_for_denied_path(path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if path.is_dir() {
        return forge_sandbox::grantable_write_dir(path);
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no directory", path.display()))?;
    forge_sandbox::grantable_write_dir(parent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_grant_holds_while_the_session_does() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = outside.path().canonicalize().unwrap();
        let grants = SessionWriteGrants::register(workspace.path());
        grants.grant(dir.clone());
        assert!(write_grants_for(workspace.path()).contains(&dir));
        let other = tempfile::tempdir().unwrap();
        assert!(!write_grants_for(other.path()).contains(&dir));
        drop(grants);
        assert!(!write_grants_for(workspace.path()).contains(&dir));
    }

    #[test]
    fn a_denied_file_asks_for_its_directory() {
        let outside = tempfile::tempdir().unwrap();
        let dir = outside.path().canonicalize().unwrap();
        let file = dir.join("cache.bin");
        assert_eq!(grant_for_denied_path(file.to_str().unwrap()).unwrap(), dir);
        assert_eq!(grant_for_denied_path(dir.to_str().unwrap()).unwrap(), dir);
        assert!(grant_for_denied_path("/etc/hosts").is_err());
    }
}
