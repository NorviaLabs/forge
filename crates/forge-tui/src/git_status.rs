//! Lightweight Git status cache for the file explorer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusKind {
    Modified,
    Added,
    Untracked,
    Conflicted,
}

impl GitStatusKind {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Untracked => "?",
            Self::Conflicted => "U",
        }
    }

    pub fn is_more_severe(self, other: Self) -> bool {
        let rank = |k: Self| match k {
            Self::Conflicted => 3,
            Self::Added => 2,
            Self::Modified => 1,
            Self::Untracked => 0,
        };
        rank(self) > rank(other)
    }
}

#[derive(Debug)]
pub struct GitStatusCache {
    /// Repository-root-relative canonical paths to status.
    pub status: HashMap<PathBuf, GitStatusKind>,
    /// Whether a refresh is currently in flight.
    pub loading: bool,
    /// Last refresh error, if any.
    pub error: Option<String>,
    pending: Option<Receiver<Result<HashMap<PathBuf, GitStatusKind>, String>>>,
}

impl GitStatusCache {
    pub fn new() -> Self {
        Self {
            status: HashMap::new(),
            loading: false,
            error: None,
            pending: None,
        }
    }

    /// Start a background refresh of the Git status for `root`.
    /// This is fully non-blocking: any previous in-flight refresh is dropped
    /// without waiting for its thread to finish.
    pub fn start_refresh(&mut self, root: PathBuf) {
        self.loading = true;
        self.error = None;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_git_status(&root));
        });
        self.pending = Some(rx);
    }

    /// Check whether the pending refresh has completed and update the cache.
    /// This is non-blocking and safe to call from the render loop.
    pub fn poll(&mut self) {
        let Some(rx) = self.pending.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(map)) => {
                self.loading = false;
                self.status = map;
            }
            Ok(Err(err)) => {
                self.loading = false;
                self.error = Some(err);
            }
            Err(TryRecvError::Empty) => {
                self.pending = Some(rx);
            }
            Err(TryRecvError::Disconnected) => {
                self.loading = false;
                self.error = Some("Git status refresh disconnected".into());
            }
        }
    }

    pub fn get(&self, path: &Path) -> Option<GitStatusKind> {
        self.status.get(path).copied()
    }
}

fn load_git_status(root: &Path) -> Result<HashMap<PathBuf, GitStatusKind>, String> {
    if !root.join(".git").exists() {
        return Ok(HashMap::new());
    }
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=1", "-z", "-uall"])
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git status: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git status failed: {stderr}"));
    }
    parse_null_terminated(&output.stdout)
}

fn parse_null_terminated(data: &[u8]) -> Result<HashMap<PathBuf, GitStatusKind>, String> {
    let mut map = HashMap::new();
    let text = String::from_utf8_lossy(data);
    let parts: Vec<&str> = text.split('\0').collect();
    let mut i = 0;
    while i < parts.len() {
        let record = parts[i];
        if record.is_empty() {
            i += 1;
            continue;
        }
        if record.len() < 3 {
            i += 1;
            continue;
        }
        let xy = &record[..2];
        let path = record[2..].trim_start_matches(' ');
        let status = classify_status(xy);
        if status.is_none() {
            i += 1;
            continue;
        }
        let status = status.unwrap();

        let (target_path, consumed) = if xy.starts_with('R') || xy.starts_with('C') {
            // Rename/copy format: "XY old_path\0new_path\0".
            if i + 1 < parts.len() {
                i += 1;
                (parts[i], 2)
            } else {
                (path, 1)
            }
        } else {
            (path, 1)
        };

        let target = PathBuf::from(target_path);
        map.entry(target)
            .and_modify(|existing| {
                if status.is_more_severe(*existing) {
                    *existing = status;
                }
            })
            .or_insert(status);
        i += consumed;
    }
    Ok(map)
}

fn classify_status(xy: &str) -> Option<GitStatusKind> {
    if xy.len() != 2 {
        return None;
    }
    let mut chars = xy.chars();
    let x = chars.next().unwrap();
    let y = chars.next().unwrap();

    if x == '?' && y == '?' {
        return Some(GitStatusKind::Untracked);
    }
    if x == '!' || y == '!' {
        return None;
    }
    if xy == "DD" || xy == "AU" || xy == "UA" || xy == "DU" || xy == "UD" || x == 'U' || y == 'U' {
        return Some(GitStatusKind::Conflicted);
    }
    if x == 'A' || y == 'A' {
        return Some(GitStatusKind::Added);
    }
    if x == 'M' || y == 'M' || x == 'R' || y == 'R' || x == 'T' || y == 'T' || x == 'C' || y == 'C'
    {
        return Some(GitStatusKind::Modified);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn classify_status_maps_common_codes() {
        assert_eq!(classify_status("M "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status(" M"), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("A "), Some(GitStatusKind::Added));
        assert_eq!(classify_status("??"), Some(GitStatusKind::Untracked));
        assert_eq!(classify_status("U "), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("!!"), None);
    }

    #[test]
    fn parse_null_terminated_simple() {
        let data = b"M a.txt\0?? b.txt\0";
        let map = parse_null_terminated(data).unwrap();
        assert_eq!(map.get(Path::new("a.txt")), Some(&GitStatusKind::Modified));
        assert_eq!(map.get(Path::new("b.txt")), Some(&GitStatusKind::Untracked));
    }

    #[test]
    fn parse_null_terminated_rename() {
        let data = b"R old.txt\0new.txt\0";
        let map = parse_null_terminated(data).unwrap();
        assert_eq!(map.get(Path::new("old.txt")), None);
        assert_eq!(
            map.get(Path::new("new.txt")),
            Some(&GitStatusKind::Modified)
        );
    }

    #[tokio::test]
    async fn load_git_status_reads_repo() {
        let root = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(root.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "forge@test"])
            .current_dir(root.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Forge Test"])
            .current_dir(root.path())
            .output()
            .unwrap();
        std::fs::write(root.path().join("tracked.txt"), "x").unwrap();
        std::fs::write(root.path().join("untracked.txt"), "y").unwrap();
        Command::new("git")
            .args(["add", "tracked.txt"])
            .current_dir(root.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root.path())
            .output()
            .unwrap();
        std::fs::write(root.path().join("tracked.txt"), "changed").unwrap();

        let map = load_git_status(root.path()).unwrap();
        assert_eq!(
            map.get(Path::new("tracked.txt")),
            Some(&GitStatusKind::Modified)
        );
        assert_eq!(
            map.get(Path::new("untracked.txt")),
            Some(&GitStatusKind::Untracked)
        );
    }

    #[tokio::test]
    async fn load_git_status_returns_empty_outside_git() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "x").unwrap();
        let map = load_git_status(root.path()).unwrap();
        assert!(map.is_empty());
    }
}
