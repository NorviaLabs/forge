//! Lightweight Git status cache for the file explorer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusKind {
    Modified,
    Added,
    Deleted,
    Untracked,
    Ignored,
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathStatus {
    pub staged: Option<GitStatusKind>,
    pub unstaged: Option<GitStatusKind>,
}

impl PathStatus {
    pub fn primary(self) -> Option<GitStatusKind> {
        match (self.staged, self.unstaged) {
            (Some(a), Some(b)) => Some(if a.is_more_severe(b) { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        }
    }

    pub fn is_untracked(self) -> bool {
        self.unstaged == Some(GitStatusKind::Untracked)
    }

    pub fn is_conflicted(self) -> bool {
        self.staged == Some(GitStatusKind::Conflicted)
            || self.unstaged == Some(GitStatusKind::Conflicted)
    }
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: PathBuf,
    pub staged: Option<GitStatusKind>,
    pub unstaged: Option<GitStatusKind>,
}

impl GitStatusKind {
    pub fn marker(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Untracked => "?",
            Self::Ignored => "!",
            Self::Conflicted => "U",
        }
    }

    pub fn is_more_severe(self, other: Self) -> bool {
        let rank = |k: Self| match k {
            Self::Conflicted => 3,
            Self::Added => 2,
            Self::Modified => 1,
            Self::Deleted => 1,
            Self::Untracked | Self::Ignored => 0,
        };
        rank(self) > rank(other)
    }
}

#[derive(Debug)]
pub struct GitStatusCache {
    /// Repository-root-relative paths to the primary (display) status.
    pub status: HashMap<PathBuf, GitStatusKind>,
    /// Staged vs worktree split for the same paths.
    pub details: HashMap<PathBuf, PathStatus>,
    /// Whether a refresh is currently in flight.
    pub loading: bool,
    /// Last refresh error, if any.
    pub error: Option<String>,
    pending: Option<Receiver<Result<HashMap<PathBuf, PathStatus>, String>>>,
    revision: u64,
    diff_cache: RefCell<HashMap<(u64, PathBuf), Result<String, String>>>,
}

impl Default for GitStatusCache {
    fn default() -> Self {
        Self::new()
    }
}

impl GitStatusCache {
    pub fn new() -> Self {
        Self {
            status: HashMap::new(),
            details: HashMap::new(),
            loading: false,
            error: None,
            pending: None,
            revision: 0,
            diff_cache: RefCell::new(HashMap::new()),
        }
    }

    /// Start a background refresh of the Git status for `root`.
    /// This is fully non-blocking: any previous in-flight refresh is dropped
    /// without waiting for its thread to finish. The previous `status` is
    /// kept until the refresh resolves, so callers that re-trigger a refresh
    /// on every navigation step (e.g. expanding a directory) don't flash
    /// stale markers to empty for a frame.
    pub fn start_refresh(&mut self, root: PathBuf) {
        self.loading = true;
        self.error = None;
        self.revision = self.revision.wrapping_add(1);
        self.diff_cache.borrow_mut().clear();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(load_git_status(&root));
        });
        self.pending = Some(rx);
    }

    /// Check whether the pending refresh has completed and update the cache.
    /// This is non-blocking and safe to call from the render loop.
    ///
    /// Returns `true` when a refresh resolved this call (successfully or not),
    /// so callers can react to freshly-landed status (e.g. re-checking diff
    /// staleness) instead of on every raw filesystem-watch event, which fires
    /// well before the async refresh it triggered has actually completed.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = self.pending.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(details)) => {
                self.loading = false;
                self.details = details;
                self.status = self
                    .details
                    .iter()
                    .filter_map(|(path, detail)| detail.primary().map(|kind| (path.clone(), kind)))
                    .collect();
                true
            }
            Ok(Err(err)) => {
                self.loading = false;
                self.error = Some(err);
                true
            }
            Err(TryRecvError::Empty) => {
                self.pending = Some(rx);
                false
            }
            Err(TryRecvError::Disconnected) => {
                self.loading = false;
                self.error = Some("Git status refresh disconnected".into());
                true
            }
        }
    }

    pub fn get(&self, path: &Path) -> Option<GitStatusKind> {
        self.status.get(path).copied()
    }

    /// Returns changed files with staged and unstaged status distinguished.
    pub fn changed_files(&self) -> Vec<ChangedFile> {
        let mut files = Vec::new();
        if !self.details.is_empty() {
            for (path, detail) in &self.details {
                files.push(ChangedFile {
                    path: path.clone(),
                    staged: detail.staged,
                    unstaged: detail.unstaged,
                });
            }
        } else {
            for (path, status) in &self.status {
                files.push(ChangedFile {
                    path: path.clone(),
                    staged: None,
                    unstaged: Some(*status),
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    pub fn path_status(&self, path: &Path) -> Option<PathStatus> {
        self.details.get(path).copied().or_else(|| {
            self.status.get(path).map(|kind| PathStatus {
                staged: None,
                unstaged: Some(*kind),
            })
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the unstaged unified diff for one path.
    pub fn get_unstaged_diff(&self, root: &Path, path: &Path) -> Result<String, String> {
        let key = (self.revision, path.to_path_buf());
        if let Some(diff) = self.diff_cache.borrow().get(&key) {
            return diff.clone();
        }
        let diff = load_unstaged_diff(root, path);
        self.diff_cache.borrow_mut().insert(key, diff.clone());
        diff
    }
}

fn load_unstaged_diff(root: &Path, path: &Path) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(["diff", "--no-color", "--", path.to_str().unwrap_or("")])
        .current_dir(root)
        .output()
        .map_err(|e| format!("failed to run git diff: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {stderr}"));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("diff output is not valid UTF-8: {e}"))
}

fn load_git_status(root: &Path) -> Result<HashMap<PathBuf, PathStatus>, String> {
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

fn parse_null_terminated(data: &[u8]) -> Result<HashMap<PathBuf, PathStatus>, String> {
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
        let Some(status) = classify_path_status(xy) else {
            i += 1;
            continue;
        };

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
            .and_modify(|existing| *existing = merge_path_status(*existing, status))
            .or_insert(status);
        i += consumed;
    }
    Ok(map)
}

fn classify_letter(letter: char) -> Option<GitStatusKind> {
    match letter {
        'M' | 'T' | 'R' | 'C' => Some(GitStatusKind::Modified),
        'A' => Some(GitStatusKind::Added),
        'D' => Some(GitStatusKind::Deleted),
        'U' => Some(GitStatusKind::Conflicted),
        '?' => Some(GitStatusKind::Untracked),
        '!' => Some(GitStatusKind::Ignored),
        _ => None,
    }
}

fn classify_path_status(xy: &str) -> Option<PathStatus> {
    if xy.len() != 2 {
        return None;
    }
    let mut chars = xy.chars();
    let x = chars.next().unwrap();
    let y = chars.next().unwrap();
    if xy == "DD" || xy == "AU" || xy == "UA" || xy == "DU" || xy == "UD" || x == 'U' || y == 'U' {
        return Some(PathStatus {
            staged: Some(GitStatusKind::Conflicted),
            unstaged: Some(GitStatusKind::Conflicted),
        });
    }
    if x == '?' && y == '?' {
        return Some(PathStatus {
            staged: None,
            unstaged: Some(GitStatusKind::Untracked),
        });
    }
    Some(PathStatus {
        staged: if x == ' ' { None } else { classify_letter(x) },
        unstaged: if y == ' ' { None } else { classify_letter(y) },
    })
    .filter(|status| status.primary().is_some())
}

fn merge_path_status(left: PathStatus, right: PathStatus) -> PathStatus {
    PathStatus {
        staged: pick_more_severe(left.staged, right.staged),
        unstaged: pick_more_severe(left.unstaged, right.unstaged),
    }
}

fn pick_more_severe(
    left: Option<GitStatusKind>,
    right: Option<GitStatusKind>,
) -> Option<GitStatusKind> {
    match (left, right) {
        (Some(a), Some(b)) => Some(if a.is_more_severe(b) { a } else { b }),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

#[cfg(test)]
fn classify_status(xy: &str) -> Option<GitStatusKind> {
    classify_path_status(xy).and_then(PathStatus::primary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn porcelain_splits_staged_and_unstaged() {
        let status = classify_path_status("MM").unwrap();
        assert_eq!(status.staged, Some(GitStatusKind::Modified));
        assert_eq!(status.unstaged, Some(GitStatusKind::Modified));
        let staged_only = classify_path_status("M ").unwrap();
        assert_eq!(staged_only.staged, Some(GitStatusKind::Modified));
        assert_eq!(staged_only.unstaged, None);
        let worktree_only = classify_path_status(" M").unwrap();
        assert_eq!(worktree_only.staged, None);
        assert_eq!(worktree_only.unstaged, Some(GitStatusKind::Modified));
    }

    #[test]
    fn classify_status_maps_common_codes() {
        assert_eq!(classify_status("M "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status(" M"), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("A "), Some(GitStatusKind::Added));
        assert_eq!(classify_status("??"), Some(GitStatusKind::Untracked));
        assert_eq!(classify_status("U "), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("!!"), Some(GitStatusKind::Ignored));
    }

    #[test]
    fn parse_null_terminated_simple() {
        let data = b"M a.txt\0?? b.txt\0";
        let map = parse_null_terminated(data).unwrap();
        assert_eq!(
            map.get(Path::new("a.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
        assert_eq!(
            map.get(Path::new("b.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Untracked)
        );
    }

    #[test]
    fn parse_null_terminated_rename() {
        let data = b"R old.txt\0new.txt\0";
        let map = parse_null_terminated(data).unwrap();
        assert_eq!(map.get(Path::new("old.txt")), None);
        assert_eq!(
            map.get(Path::new("new.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
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
            map.get(Path::new("tracked.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
        assert_eq!(
            map.get(Path::new("untracked.txt"))
                .and_then(|s| s.primary()),
            Some(GitStatusKind::Untracked)
        );
    }

    #[tokio::test]
    async fn load_git_status_returns_empty_outside_git() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), "x").unwrap();
        let map = load_git_status(root.path()).unwrap();
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn load_git_status_handles_spaces_and_unicode() {
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
        std::fs::write(root.path().join("file with spaces.txt"), "x").unwrap();
        std::fs::write(root.path().join("文件.txt"), "y").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(root.path())
            .output()
            .unwrap();

        let map = load_git_status(root.path()).unwrap();
        assert_eq!(
            map.get(Path::new("file with spaces.txt"))
                .and_then(|s| s.primary()),
            Some(GitStatusKind::Added)
        );
        assert_eq!(
            map.get(Path::new("文件.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Added)
        );
    }

    #[test]
    fn cache_replaces_status_on_refresh_coalescing() {
        fn make_repo_with(name: &str) -> tempfile::TempDir {
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
            std::fs::write(root.path().join(name), "x").unwrap();
            Command::new("git")
                .args(["add", name])
                .current_dir(root.path())
                .output()
                .unwrap();
            root
        }

        let first = make_repo_with("first.txt");
        let second = make_repo_with("second.txt");
        let mut cache = GitStatusCache::new();

        cache.start_refresh(first.path().to_path_buf());
        // Start a second refresh while the first is in flight. This must not block.
        cache.start_refresh(second.path().to_path_buf());
        while cache.loading {
            cache.poll();
        }

        assert!(cache.status.contains_key(Path::new("second.txt")));
        assert!(!cache.status.contains_key(Path::new("first.txt")));
        assert!(cache.error.is_none());
    }

    #[tokio::test]
    async fn load_git_status_reports_git_failure() {
        let root = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(root.path())
            .output()
            .unwrap();
        // Corrupt the repository so `git status` fails.
        std::fs::remove_file(root.path().join(".git/HEAD")).unwrap();
        let result = load_git_status(root.path());
        assert!(result.is_err());
    }

    #[test]
    fn clean_file_has_no_marker() {
        let mut map = HashMap::new();
        map.insert(PathBuf::from("dirty.txt"), GitStatusKind::Modified);
        // A file not present in the status map is considered clean.
        assert!(!map.contains_key(Path::new("clean.txt")));
    }

    #[test]
    fn every_kind_has_a_distinct_marker() {
        use GitStatusKind::*;
        let pairs = [
            (Modified, "M"),
            (Added, "A"),
            (Deleted, "D"),
            (Untracked, "?"),
            (Ignored, "!"),
            (Conflicted, "U"),
        ];
        for (kind, expected) in pairs {
            assert_eq!(kind.marker(), expected, "{kind:?} has the wrong marker");
        }
        let distinct: std::collections::HashSet<&str> =
            pairs.iter().map(|(kind, _)| kind.marker()).collect();
        assert_eq!(distinct.len(), pairs.len(), "markers must be unambiguous");
    }

    #[test]
    fn severity_ranks_conflicts_above_adds_above_edits() {
        use GitStatusKind::*;
        assert!(Conflicted.is_more_severe(Added));
        assert!(Added.is_more_severe(Modified));
        assert!(Modified.is_more_severe(Untracked));
        assert!(!Added.is_more_severe(Conflicted));
        // Modified and Deleted share a rank, as do Untracked and Ignored.
        assert!(!Modified.is_more_severe(Deleted));
        assert!(!Deleted.is_more_severe(Modified));
        assert!(!Untracked.is_more_severe(Ignored));
    }

    #[test]
    fn poll_without_a_pending_refresh_is_a_noop() {
        let mut cache = GitStatusCache::new();
        cache.poll();
        assert!(!cache.loading);
        assert!(cache.error.is_none());
        assert!(cache.status.is_empty());
    }

    #[test]
    fn diff_cache_is_keyed_by_revision_and_path() {
        let root = tempfile::tempdir().unwrap();
        Command::new("git")
            .arg("init")
            .current_dir(root.path())
            .output()
            .unwrap();
        let path = PathBuf::from("cached.txt");
        let key = (0, path.clone());
        let mut cache = GitStatusCache::new();
        cache
            .diff_cache
            .borrow_mut()
            .insert(key, Ok("cached diff".to_string()));

        assert_eq!(
            cache.get_unstaged_diff(root.path(), &path).unwrap(),
            "cached diff"
        );

        cache.start_refresh(root.path().to_path_buf());
        assert_eq!(cache.get_unstaged_diff(root.path(), &path).unwrap(), "");
    }

    #[test]
    fn poll_records_a_refresh_error() {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err("git exploded".to_string())).unwrap();
        let mut cache = GitStatusCache::new();
        cache.loading = true;
        cache.pending = Some(rx);

        cache.poll();

        assert!(!cache.loading);
        assert_eq!(cache.error.as_deref(), Some("git exploded"));
        assert!(cache.status.is_empty());
    }

    #[test]
    fn poll_reports_a_dropped_sender() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<HashMap<PathBuf, PathStatus>, String>>();
        drop(tx);
        let mut cache = GitStatusCache::new();
        cache.loading = true;
        cache.pending = Some(rx);

        cache.poll();

        assert!(!cache.loading);
        assert_eq!(
            cache.error.as_deref(),
            Some("Git status refresh disconnected")
        );
    }

    #[test]
    fn poll_retains_the_receiver_while_a_refresh_is_still_running() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<HashMap<PathBuf, PathStatus>, String>>();
        let mut cache = GitStatusCache::new();
        cache.loading = true;
        cache.pending = Some(rx);

        // Nothing sent yet: the refresh stays in flight and the receiver is kept.
        cache.poll();
        assert!(cache.loading);
        assert!(cache.pending.is_some());

        let mut map = HashMap::new();
        map.insert(
            PathBuf::from("late.txt"),
            PathStatus {
                staged: Some(GitStatusKind::Added),
                unstaged: None,
            },
        );
        tx.send(Ok(map)).unwrap();
        cache.poll();

        assert!(!cache.loading);
        assert_eq!(cache.get(Path::new("late.txt")), Some(GitStatusKind::Added));
    }

    #[test]
    fn revision_advances_once_per_refresh() {
        let root = tempfile::tempdir().unwrap();
        let mut cache = GitStatusCache::new();
        assert_eq!(cache.revision(), 0);

        cache.start_refresh(root.path().to_path_buf());
        assert_eq!(cache.revision(), 1);
        while cache.loading {
            cache.poll();
        }

        cache.start_refresh(root.path().to_path_buf());
        assert_eq!(cache.revision(), 2);
        while cache.loading {
            cache.poll();
        }
    }

    #[test]
    fn changed_files_are_sorted_and_reported_as_unstaged() {
        let mut cache = GitStatusCache::new();
        cache
            .status
            .insert(PathBuf::from("z.txt"), GitStatusKind::Added);
        cache
            .status
            .insert(PathBuf::from("a.txt"), GitStatusKind::Modified);

        let files = cache.changed_files();

        assert_eq!(
            files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            vec![PathBuf::from("a.txt"), PathBuf::from("z.txt")]
        );
        assert_eq!(files[0].unstaged, Some(GitStatusKind::Modified));
        assert!(files.iter().all(|f| f.staged.is_none()));
    }

    #[test]
    fn classify_status_rejects_codes_that_are_not_two_chars() {
        assert_eq!(classify_status(""), None);
        assert_eq!(classify_status("M"), None);
        assert_eq!(classify_status("MMM"), None);
    }

    #[test]
    fn classify_status_maps_remaining_codes() {
        assert_eq!(classify_status("D "), Some(GitStatusKind::Deleted));
        assert_eq!(classify_status(" D"), Some(GitStatusKind::Deleted));
        assert_eq!(classify_status("T "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("C "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("R "), Some(GitStatusKind::Modified));
        assert_eq!(classify_status("DD"), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("AU"), Some(GitStatusKind::Conflicted));
        assert_eq!(classify_status("UD"), Some(GitStatusKind::Conflicted));
        // A clean entry and an unrecognised pair both classify as nothing.
        assert_eq!(classify_status("  "), None);
        assert_eq!(classify_status("XY"), None);
    }

    #[test]
    fn parse_null_terminated_skips_malformed_records() {
        // "M" is below the 3-byte "XY path" minimum and "ZZ x.txt" carries an
        // unrecognised status pair. Both are skipped without failing the parse.
        let map = parse_null_terminated(b"M\0ZZ x.txt\0M  good.txt\0").unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.get(Path::new("good.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
    }

    #[test]
    fn duplicate_paths_keep_the_more_severe_status() {
        // The severity comparison must win regardless of the order git reports in.
        let escalating = parse_null_terminated(b"M  dup.txt\0A  dup.txt\0").unwrap();
        assert_eq!(
            escalating
                .get(Path::new("dup.txt"))
                .and_then(|s| s.primary()),
            Some(GitStatusKind::Added)
        );
        let descending = parse_null_terminated(b"A  dup.txt\0M  dup.txt\0").unwrap();
        assert_eq!(
            descending
                .get(Path::new("dup.txt"))
                .and_then(|s| s.primary()),
            Some(GitStatusKind::Added)
        );
    }

    #[test]
    fn truncated_rename_record_falls_back_to_the_original_path() {
        // A rename record whose second (new path) entry never arrives must not
        // panic, and should fall back to the path carried by the first record.
        let map = parse_null_terminated(b"R  only.txt").unwrap();
        assert_eq!(
            map.get(Path::new("only.txt")).and_then(|s| s.primary()),
            Some(GitStatusKind::Modified)
        );
    }
}
