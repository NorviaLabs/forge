//! Native Git exclusion via the repository-local exclude file
//! (`git rev-parse --git-path info/exclude`), managed as a single
//! idempotent block. Never touches the committed `.gitignore` — this is
//! checkout-local, not a project convention.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

const BEGIN_MARKER: &str = "# BEGIN Forge";
const END_MARKER: &str = "# END Forge";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExcludeError {
    #[error("could not resolve the git exclude file path for this repository")]
    PathResolutionFailed,
    #[error("io error managing the git exclude file: {0}")]
    Io(#[from] io::Error),
}

/// Resolve the repository-local exclude file path for `workspace` through
/// Git itself, rather than assuming `.git/info/exclude` — this is what
/// makes worktrees, relocated Git directories, and `.git`-as-a-file setups
/// resolve to the correct file.
pub fn resolve_exclude_path(workspace: &Path) -> Result<PathBuf, ExcludeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .map_err(|_| ExcludeError::PathResolutionFailed)?;
    if !output.status.success() {
        return Err(ExcludeError::PathResolutionFailed);
    }
    let text = String::from_utf8(output.stdout).map_err(|_| ExcludeError::PathResolutionFailed)?;
    let rel = text.trim();
    if rel.is_empty() {
        return Err(ExcludeError::PathResolutionFailed);
    }
    let path = PathBuf::from(rel);
    Ok(if path.is_relative() {
        workspace.join(path)
    } else {
        path
    })
}

/// Idempotently ensure the managed Forge block in the exclude file at
/// `exclude_path` contains exactly `pattern`. Preserves every unrelated
/// line verbatim (no sorting/reformatting), tolerates a missing trailing
/// newline, and reconciles any pre-existing duplicate Forge blocks into
/// one. Writes atomically (temp file in the same directory, then rename).
pub fn ensure_managed_block(exclude_path: &Path, pattern: &str) -> Result<(), ExcludeError> {
    let existing = fs::read_to_string(exclude_path).unwrap_or_default();

    let mut kept_lines: Vec<&str> = Vec::new();
    let mut in_block = false;
    for line in existing.lines() {
        match line.trim() {
            _ if line.trim() == BEGIN_MARKER => in_block = true,
            _ if line.trim() == END_MARKER => in_block = false,
            _ if in_block => {}
            _ => kept_lines.push(line),
        }
    }
    // Drop trailing blank lines a removed block would otherwise leave behind,
    // so repeated calls don't accumulate blank lines.
    while kept_lines.last().is_some_and(|l| l.trim().is_empty()) {
        kept_lines.pop();
    }

    let mut new_content = kept_lines.join("\n");
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(BEGIN_MARKER);
    new_content.push('\n');
    new_content.push_str(pattern);
    new_content.push('\n');
    new_content.push_str(END_MARKER);
    new_content.push('\n');

    if new_content == existing {
        return Ok(());
    }

    write_atomic(exclude_path, &new_content)
}

/// True if the exclude file already has an active Forge block covering
/// `pattern` — used to verify the rule is in place without rewriting.
pub fn has_managed_block(exclude_path: &Path, pattern: &str) -> bool {
    let Ok(content) = fs::read_to_string(exclude_path) else {
        return false;
    };
    let mut in_block = false;
    for line in content.lines() {
        match line.trim() {
            _ if line.trim() == BEGIN_MARKER => in_block = true,
            _ if line.trim() == END_MARKER => in_block = false,
            l if in_block && l == pattern => return true,
            _ => {}
        }
    }
    false
}

fn write_atomic(path: &Path, content: &str) -> Result<(), ExcludeError> {
    let dir = path.parent().ok_or(ExcludeError::PathResolutionFailed)?;
    fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!(".forge-exclude-{}.tmp", std::process::id()));
    fs::write(&tmp_path, content)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn adds_the_block_to_an_empty_or_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains(BEGIN_MARKER));
        assert!(content.contains("/.forge/local/"));
        assert!(content.contains(END_MARKER));
    }

    #[test]
    fn preserves_existing_patterns_and_comments() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        fs::write(&path, "# my notes\n*.bak\n").unwrap();
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# my notes"));
        assert!(content.contains("*.bak"));
        assert!(content.contains("/.forge/local/"));
    }

    #[test]
    fn handles_missing_trailing_newline() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        fs::write(&path, "*.bak").unwrap(); // no trailing newline
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("*.bak"));
        assert!(content.contains("/.forge/local/"));
    }

    #[test]
    fn is_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let first = fs::read_to_string(&path).unwrap();
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let second = fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn reconciles_duplicate_forge_blocks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        fs::write(
            &path,
            format!("{BEGIN_MARKER}\n/.forge/local/\n{END_MARKER}\n{BEGIN_MARKER}\n/.forge/local/\n{END_MARKER}\n"),
        )
        .unwrap();
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content.matches(BEGIN_MARKER).count(), 1);
        assert_eq!(content.matches(END_MARKER).count(), 1);
    }

    #[test]
    fn never_excludes_all_of_dot_forge() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.lines().any(|l| l.trim() == "/.forge/"));
        assert!(!content.lines().any(|l| l.trim() == ".forge*"));
    }

    #[test]
    fn has_managed_block_detects_an_active_rule() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        assert!(!has_managed_block(&path, "/.forge/local/"));
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        assert!(has_managed_block(&path, "/.forge/local/"));
    }

    #[test]
    fn fails_safely_on_an_unwritable_directory() {
        // Point at a path whose parent cannot be created (a file where a
        // directory is expected) rather than relying on chmod, which is
        // unreliable to assert on in CI across platforms.
        let dir = tempdir().unwrap();
        let blocker = dir.path().join("not-a-dir");
        fs::write(&blocker, "x").unwrap();
        let path = blocker.join("info").join("exclude");
        let result = ensure_managed_block(&path, "/.forge/local/");
        assert!(result.is_err());
    }

    #[test]
    fn leaves_unrelated_bytes_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("exclude");
        let original = "*.log\n/build/\n# keep me\n";
        fs::write(&path, original).unwrap();
        ensure_managed_block(&path, "/.forge/local/").unwrap();
        let content = fs::read_to_string(&path).unwrap();
        for line in original.lines() {
            assert!(content.contains(line), "missing original line: {line}");
        }
    }
}
