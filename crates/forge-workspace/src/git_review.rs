//! Worktree review operations: combined dirty-vs-HEAD diffs and discard.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git_status::PathStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub header: String,
    pub lines: Vec<String>,
}

impl DiffHunk {
    pub fn id(&self) -> &str {
        &self.header
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    pub headers: Vec<String>,
    pub hunks: Vec<DiffHunk>,
    pub binary: bool,
    pub untracked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reviewability {
    Reviewable,
    Conflicted,
    Binary,
}

pub fn reviewability(status: PathStatus, diff: &FileDiff) -> Reviewability {
    if status.is_conflicted() {
        Reviewability::Conflicted
    } else if diff.binary {
        Reviewability::Binary
    } else {
        Reviewability::Reviewable
    }
}

/// Dirty worktree + index vs `HEAD`. Untracked files are a synthetic add diff.
pub fn combined_diff(root: &Path, path: &Path) -> Result<FileDiff, String> {
    let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let (output, untracked) = combined_diff_text(root, path)?;
    Ok(parse_file_diff(rel, &output, untracked))
}

/// The raw `git` output behind [`combined_diff`], plus whether the path was
/// untracked (which decides how [`parse_file_diff`] reads it).
///
/// Split out so callers that cache diff text across a status revision — the
/// `/diff` view — can hold the cheap `String` and parse on demand, instead of
/// keeping a parsed [`FileDiff`] per changed file.
pub fn combined_diff_text(root: &Path, path: &Path) -> Result<(String, bool), String> {
    let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
    let tracked = is_tracked(root, &rel);
    let (output, untracked) = if tracked {
        (
            run_git(
                root,
                &["diff", "--no-color", "HEAD", "--", &rel.to_string_lossy()],
                false,
            )?,
            false,
        )
    } else {
        let abs = root.join(&rel);
        (
            run_git(
                root,
                &[
                    "diff",
                    "--no-color",
                    "--no-index",
                    "--",
                    "/dev/null",
                    &abs.to_string_lossy(),
                ],
                true,
            )?,
            true,
        )
    };
    Ok((output, untracked))
}

pub fn discard_hunk(root: &Path, path: &Path, hunk_index: usize) -> Result<(), String> {
    let diff = combined_diff(root, path)?;
    if diff.untracked {
        return Err("untracked files must be deleted, not reverse-applied".into());
    }
    if diff.binary {
        return Err("binary files cannot be discarded by hunk".into());
    }
    let hunk = diff
        .hunks
        .get(hunk_index)
        .ok_or_else(|| format!("hunk {hunk_index} is out of range"))?;
    apply_reverse_hunk(root, &diff, hunk)
}

pub fn restore_path(root: &Path, path: &Path) -> Result<(), String> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if !is_tracked(root, rel) {
        return Err("untracked files cannot be restored to HEAD".into());
    }
    run_git(
        root,
        &[
            "restore",
            "--source=HEAD",
            "--worktree",
            "--staged",
            "--",
            &rel.to_string_lossy(),
        ],
        false,
    )?;
    Ok(())
}

pub fn delete_untracked(root: &Path, path: &Path) -> Result<(), String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let canon_root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    let canon = abs
        .canonicalize()
        .map_err(|e| format!("cannot delete {}: {e}", abs.display()))?;
    if !canon.starts_with(&canon_root) {
        return Err("refusing to delete a path outside the workspace".into());
    }
    if is_tracked(root, abs.strip_prefix(root).unwrap_or(&abs)) {
        return Err("refusing to delete a tracked path".into());
    }
    let meta =
        fs::symlink_metadata(&canon).map_err(|e| format!("stat {}: {e}", canon.display()))?;
    if meta.is_dir() {
        fs::remove_dir_all(&canon).map_err(|e| format!("delete dir {}: {e}", canon.display()))?;
    } else {
        fs::remove_file(&canon).map_err(|e| format!("delete {}: {e}", canon.display()))?;
    }
    Ok(())
}

pub fn parse_file_diff(path: PathBuf, diff: &str, untracked: bool) -> FileDiff {
    let binary = diff.contains("Binary files ") || diff.contains("GIT binary patch");
    let mut headers = Vec::new();
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    for line in diff.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(DiffHunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
        } else if let Some(hunk) = current.as_mut() {
            hunk.lines.push(line.to_string());
        } else {
            headers.push(line.to_string());
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    FileDiff {
        path,
        headers,
        hunks,
        binary,
        untracked,
    }
}

fn apply_reverse_hunk(root: &Path, diff: &FileDiff, hunk: &DiffHunk) -> Result<(), String> {
    let mut patch = String::new();
    if diff
        .headers
        .iter()
        .any(|line| line.starts_with("diff --git"))
    {
        for line in &diff.headers {
            patch.push_str(line);
            patch.push('\n');
        }
    } else {
        let display = diff.path.display();
        patch.push_str(&format!("diff --git a/{display} b/{display}\n"));
        patch.push_str(&format!("--- a/{display}\n"));
        patch.push_str(&format!("+++ b/{display}\n"));
    }
    patch.push_str(&hunk.header);
    patch.push('\n');
    for line in &hunk.lines {
        patch.push_str(line);
        patch.push('\n');
    }
    let patch_path = std::env::temp_dir().join(format!(
        "forge-review-{}-{}.patch",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&patch_path, patch).map_err(|e| format!("write patch: {e}"))?;
    let worktree = Command::new("git")
        .args([
            "apply",
            "--reverse",
            "--whitespace=nowarn",
            &patch_path.to_string_lossy(),
        ])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git apply: {e}"))?;
    if !worktree.status.success() {
        let _ = fs::remove_file(&patch_path);
        return Err(format!(
            "git apply --reverse failed: {}",
            String::from_utf8_lossy(&worktree.stderr)
        ));
    }
    // Index may already match HEAD (unstaged-only). Ignore a cached miss.
    let _ = Command::new("git")
        .args([
            "apply",
            "--reverse",
            "--cached",
            "--whitespace=nowarn",
            &patch_path.to_string_lossy(),
        ])
        .current_dir(root)
        .output();
    let _ = fs::remove_file(&patch_path);
    Ok(())
}

fn is_tracked(root: &Path, rel: &Path) -> bool {
    Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", &rel.to_string_lossy()])
        .current_dir(root)
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn run_git(root: &Path, args: &[&str], allow_one: bool) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git {} failed: {e}", args.first().copied().unwrap_or("")))?;
    let code = output.status.code().unwrap_or(1);
    if output.status.success() || (allow_one && code == 1) {
        return String::from_utf8(output.stdout)
            .map_err(|e| format!("git output is not valid UTF-8: {e}"));
    }
    Err(format!(
        "git {} failed: {}",
        args.first().copied().unwrap_or(""),
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// Add `path` to the index. Reversible with [`unstage_path`], which is why
/// `/diff` binds it without a confirmation step.
pub fn stage_path(root: &Path, path: &Path) -> Result<(), String> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    run_git(root, &["add", "--", &rel.to_string_lossy()], false)?;
    Ok(())
}

/// Remove `path` from the index, leaving the worktree untouched.
///
/// `git restore --staged` fails on a repository with no commits yet, where
/// there is no `HEAD` to restore from; `git rm --cached` is the fallback that
/// still only touches the index.
pub fn unstage_path(root: &Path, path: &Path) -> Result<(), String> {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let name = rel.to_string_lossy().to_string();
    match run_git(root, &["restore", "--staged", "--", &name], false) {
        Ok(_) => Ok(()),
        Err(_) => {
            run_git(root, &["rm", "--cached", "--quiet", "--", &name], false)?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "--initial-branch=main", "-q"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
        ] {
            assert!(Command::new("git")
                .args(&args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success());
        }
        fs::write(dir.path().join("file.txt"), "one\ntwo\nthree\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "base"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        dir
    }

    #[test]
    fn parse_file_diff_splits_hunks() {
        let diff = "\
diff --git a/file.txt b/file.txt
--- a/file.txt
+++ b/file.txt
@@ -1,2 +1,2 @@
-one
+ONE
 two
@@ -3,1 +3,1 @@
-three
+THREE
";
        let parsed = parse_file_diff(PathBuf::from("file.txt"), diff, false);
        assert_eq!(parsed.hunks.len(), 2);
        assert!(parsed.hunks[0].header.starts_with("@@ -1,2"));
        assert!(!parsed.binary);
    }

    #[test]
    fn combined_diff_includes_staged_and_unstaged() {
        let dir = repo();
        fs::write(dir.path().join("file.txt"), "one\nTWO\nthree\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
        fs::write(dir.path().join("file.txt"), "one\nTWO\nTHREE\n").unwrap();
        let diff = combined_diff(dir.path(), &dir.path().join("file.txt")).unwrap();
        let body = diff
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("+TWO"), "{body}");
        assert!(body.contains("+THREE"), "{body}");
        assert!(!diff.untracked);
    }

    #[test]
    fn discard_hunk_restores_those_lines_to_head() {
        let dir = repo();
        fs::write(dir.path().join("file.txt"), "ONE\ntwo\nthree\n").unwrap();
        let before = combined_diff(dir.path(), Path::new("file.txt")).unwrap();
        assert_eq!(before.hunks.len(), 1);
        discard_hunk(dir.path(), Path::new("file.txt"), 0).unwrap();
        let text = fs::read_to_string(dir.path().join("file.txt")).unwrap();
        assert_eq!(text, "one\ntwo\nthree\n");
    }

    #[test]
    fn restore_path_reverts_the_whole_file() {
        let dir = repo();
        fs::write(dir.path().join("file.txt"), "changed\n").unwrap();
        restore_path(dir.path(), Path::new("file.txt")).unwrap();
        let text = fs::read_to_string(dir.path().join("file.txt")).unwrap();
        assert_eq!(text, "one\ntwo\nthree\n");
    }

    #[test]
    fn delete_untracked_removes_the_file() {
        let dir = repo();
        let extra = dir.path().join("extra.txt");
        fs::write(&extra, "nope\n").unwrap();
        delete_untracked(dir.path(), Path::new("extra.txt")).unwrap();
        assert!(!extra.exists());
    }

    #[test]
    fn delete_untracked_refuses_tracked_files() {
        let dir = repo();
        let err = delete_untracked(dir.path(), Path::new("file.txt")).unwrap_err();
        assert!(err.contains("tracked"), "{err}");
        assert!(dir.path().join("file.txt").exists());
    }
}
