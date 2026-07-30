//! Lightweight Git status cache for the file explorer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

use ratatui::style::Style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusKind {
    Modified,
    Added,
    Deleted,
    Untracked,
    Ignored,
    Conflicted,
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

    /// Semantic style for the status marker. The marker character is always
    /// rendered; colour is a secondary cue.
    pub fn style(self) -> Style {
        match self {
            Self::Modified => crate::theme::info(),
            Self::Added => crate::theme::ok(),
            Self::Deleted => crate::theme::danger(),
            Self::Untracked => crate::theme::muted(),
            Self::Ignored => crate::theme::dim(),
            Self::Conflicted => crate::theme::danger(),
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
    /// Repository-root-relative canonical paths to status.
    pub status: HashMap<PathBuf, GitStatusKind>,
    /// Whether a refresh is currently in flight.
    pub loading: bool,
    /// Last refresh error, if any.
    pub error: Option<String>,
    pending: Option<Receiver<Result<HashMap<PathBuf, GitStatusKind>, String>>>,
    revision: u64,
}

impl GitStatusCache {
    pub fn new() -> Self {
        Self {
            status: HashMap::new(),
            loading: false,
            error: None,
            pending: None,
            revision: 0,
        }
    }

    /// Start a background refresh of the Git status for `root`.
    /// This is fully non-blocking: any previous in-flight refresh is dropped
    /// without waiting for its thread to finish.
    pub fn start_refresh(&mut self, root: PathBuf) {
        self.loading = true;
        self.error = None;
        self.status.clear();
        self.revision = self.revision.wrapping_add(1);
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

    /// Returns changed files with staged and unstaged status distinguished.
    pub fn changed_files(&self) -> Vec<ChangedFile> {
        let mut files = Vec::new();
        for (path, status) in &self.status {
            files.push(ChangedFile {
                path: path.clone(),
                staged: None,
                unstaged: Some(*status),
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the unstaged unified diff for one path.
    pub fn get_unstaged_diff(&self, root: &Path, path: &Path) -> Result<String, String> {
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
        return Some(GitStatusKind::Ignored);
    }
    if xy == "DD" || xy == "AU" || xy == "UA" || xy == "DU" || xy == "UD" || x == 'U' || y == 'U' {
        return Some(GitStatusKind::Conflicted);
    }
    if x == 'A' || y == 'A' {
        return Some(GitStatusKind::Added);
    }
    if x == 'D' || y == 'D' {
        return Some(GitStatusKind::Deleted);
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
        assert_eq!(classify_status("!!"), Some(GitStatusKind::Ignored));
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
            map.get(Path::new("file with spaces.txt")),
            Some(&GitStatusKind::Added)
        );
        assert_eq!(map.get(Path::new("文件.txt")), Some(&GitStatusKind::Added));
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
    fn style_follows_theme_semantics() {
        use GitStatusKind::*;
        assert_eq!(Modified.style(), crate::theme::info());
        assert_eq!(Added.style(), crate::theme::ok());
        assert_eq!(Deleted.style(), crate::theme::danger());
        assert_eq!(Untracked.style(), crate::theme::muted());
        assert_eq!(Ignored.style(), crate::theme::dim());
        assert_eq!(Conflicted.style(), crate::theme::danger());
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
        let (tx, rx) =
            std::sync::mpsc::channel::<Result<HashMap<PathBuf, GitStatusKind>, String>>();
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
        let (tx, rx) =
            std::sync::mpsc::channel::<Result<HashMap<PathBuf, GitStatusKind>, String>>();
        let mut cache = GitStatusCache::new();
        cache.loading = true;
        cache.pending = Some(rx);

        // Nothing sent yet: the refresh stays in flight and the receiver is kept.
        cache.poll();
        assert!(cache.loading);
        assert!(cache.pending.is_some());

        let mut map = HashMap::new();
        map.insert(PathBuf::from("late.txt"), GitStatusKind::Added);
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
            map.get(Path::new("good.txt")),
            Some(&GitStatusKind::Modified)
        );
    }

    #[test]
    fn duplicate_paths_keep_the_more_severe_status() {
        // The severity comparison must win regardless of the order git reports in.
        let escalating = parse_null_terminated(b"M  dup.txt\0A  dup.txt\0").unwrap();
        assert_eq!(
            escalating.get(Path::new("dup.txt")),
            Some(&GitStatusKind::Added)
        );
        let descending = parse_null_terminated(b"A  dup.txt\0M  dup.txt\0").unwrap();
        assert_eq!(
            descending.get(Path::new("dup.txt")),
            Some(&GitStatusKind::Added)
        );
    }

    #[test]
    fn truncated_rename_record_falls_back_to_the_original_path() {
        // A rename record whose second (new path) entry never arrives must not
        // panic, and should fall back to the path carried by the first record.
        let map = parse_null_terminated(b"R  only.txt").unwrap();
        assert_eq!(
            map.get(Path::new("only.txt")),
            Some(&GitStatusKind::Modified)
        );
    }
}
