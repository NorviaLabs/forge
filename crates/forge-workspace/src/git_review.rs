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
    let rel = validated_relative_path(root, path)?;
    let (output, untracked) = combined_diff_text_validated(root, &rel)?;
    Ok(parse_file_diff(rel, &output, untracked))
}

/// The raw `git` output behind [`combined_diff`], plus whether the path was
/// untracked (which decides how [`parse_file_diff`] reads it).
///
/// Split out so callers that cache diff text across a status revision — the
/// `/diff` view — can hold the cheap `String` and parse on demand, instead of
/// keeping a parsed [`FileDiff`] per changed file.
pub fn combined_diff_text(root: &Path, path: &Path) -> Result<(String, bool), String> {
    let rel = validated_relative_path(root, path)?;
    combined_diff_text_validated(root, &rel)
}

fn combined_diff_text_validated(root: &Path, rel: &Path) -> Result<(String, bool), String> {
    let rel = rel.to_path_buf();
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

/// The index against `HEAD` — the hunks a commit would take.
///
/// Distinct from [`combined_diff_text`], which reads `HEAD` against the
/// *working tree*: reviewing what you staged is a different question from
/// reviewing what you changed, and only this answers the first.
pub fn staged_diff_text(root: &Path, path: &Path) -> Result<String, String> {
    let rel = validated_relative_path(root, path)?;
    // An untracked file has nothing in the index, so it can never appear here:
    // returning git's empty output is the truthful answer, not an error.
    run_git(
        root,
        &[
            "diff",
            "--no-color",
            "--cached",
            "--",
            &rel.to_string_lossy(),
        ],
        false,
    )
}

pub fn discard_hunk(root: &Path, path: &Path, hunk_index: usize) -> Result<(), String> {
    validated_relative_path(root, path)?;
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
    let rel = validated_relative_path(root, path)?;
    if !is_tracked(root, &rel) {
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

/// Keep review subprocesses scoped to ordinary paths inside the selected worktree.
fn validated_relative_path(root: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?;
    let rel = if path.is_absolute() {
        let absolute = if path.exists() {
            path.canonicalize()
                .map_err(|e| format!("workspace path: {e}"))?
        } else {
            path.to_path_buf()
        };
        absolute
            .strip_prefix(&root)
            .map_err(|_| "path is outside the workspace")?
            .to_path_buf()
    } else {
        path.to_path_buf()
    };
    if rel.as_os_str().is_empty()
        || rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        || rel
            .components()
            .next()
            .is_some_and(|c| c.as_os_str() == ".git")
    {
        return Err("invalid workspace path".into());
    }
    let mut current = root;
    for component in rel.components() {
        current.push(component);
        if fs::symlink_metadata(&current).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("path crosses a symlink".into());
        }
    }
    Ok(rel)
}

pub fn delete_untracked(root: &Path, path: &Path) -> Result<(), String> {
    let rel = validated_relative_path(root, path)?;
    if is_tracked(root, &rel) {
        return Err("refusing to delete a tracked path".into());
    }
    let abs = root
        .canonicalize()
        .map_err(|e| format!("workspace root: {e}"))?
        .join(&rel);
    let meta = fs::symlink_metadata(&abs).map_err(|e| format!("stat {}: {e}", abs.display()))?;
    if meta.is_dir() {
        fs::remove_dir_all(&abs).map_err(|e| format!("delete dir {}: {e}", abs.display()))?;
    } else {
        fs::remove_file(&abs).map_err(|e| format!("delete {}: {e}", abs.display()))?;
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
    let worktree = git_command(root)
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
    let _ = git_command(root)
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
    git_command(root)
        .args(["ls-files", "--error-unmatch", "--", &rel.to_string_lossy()])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn git_command(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    command
}

fn run_git(root: &Path, args: &[&str], allow_one: bool) -> Result<String, String> {
    let output = git_command(root)
        .args(args)
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
    let rel = validated_relative_path(root, path)?;
    run_git(root, &["add", "--", &rel.to_string_lossy()], false)?;
    Ok(())
}

/// Remove `path` from the index, leaving the worktree untouched.
///
/// `git restore --staged` fails on a repository with no commits yet, where
/// there is no `HEAD` to restore from; `git rm --cached` is the fallback that
/// still only touches the index.
pub fn unstage_path(root: &Path, path: &Path) -> Result<(), String> {
    let rel = validated_relative_path(root, path)?;
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

    #[test]
    fn git_review_operations_reject_paths_outside_or_through_symlinks() {
        let dir = repo();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), "keep\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        assert!(stage_path(dir.path(), outside.path().join("secret").as_path()).is_err());
        #[cfg(unix)]
        {
            assert!(delete_untracked(dir.path(), Path::new("link/secret")).is_err());
            assert_eq!(
                fs::read_to_string(outside.path().join("secret")).unwrap(),
                "keep\n"
            );
        }
    }
}
