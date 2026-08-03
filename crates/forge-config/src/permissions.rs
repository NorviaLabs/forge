//! Loading for the layered permission-rule file (`permissions.toml`).
//!
//! Two scopes are read: a personal file under the user config directory and
//! a repo-committed file at `<workspace>/.forge/permissions.toml`. Both use
//! the same schema — `allow`/`deny` arrays of pattern strings — but they are
//! not trusted equally. The repo file lives in a checked-out working tree
//! that may not be trusted (the same reasoning that already restricts
//! project-discovered MCP server configuration, see the README's
//! "Configuration" section): its `allow` entries, which would let a call
//! skip an approval prompt, are ignored, while its `deny` entries, which can
//! only ever make Forge ask *more* often, are always honored. Only the
//! personal file can loosen what gets auto-approved.
//!
//! Pattern syntax (`tool(pattern)`) is interpreted by `forge_governance`,
//! not here — this module only knows how to find, read, and merge the raw
//! strings.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PermissionsFile {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
}

pub fn parse_permissions_toml(content: &str) -> Result<PermissionsFile, String> {
    toml::from_str(content).map_err(|e| e.to_string())
}

/// Personal-scope path: `<user config dir>/forge/permissions.toml`. `None`
/// when the platform config directory can't be resolved.
pub fn user_permissions_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("forge").join("permissions.toml"))
}

/// Repo-scope path: `<workspace>/.forge/permissions.toml`.
pub fn workspace_permissions_path(workspace: &Path) -> PathBuf {
    workspace.join(".forge").join("permissions.toml")
}

/// Load and merge personal + repo permission files.
///
/// Returns the merged rules plus a human-readable diagnostic for every file
/// that failed to parse, or whose `allow` entries were ignored because they
/// came from the untrusted repo scope — a bad or over-reaching file doesn't
/// silently vanish with no explanation.
pub fn load_permissions(workspace: &Path) -> (PermissionsFile, Vec<String>) {
    let mut merged = PermissionsFile::default();
    let mut diagnostics = Vec::new();

    if let Some(path) = user_permissions_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            match parse_permissions_toml(&content) {
                Ok(file) => {
                    merged.allow.extend(file.allow);
                    merged.deny.extend(file.deny);
                }
                Err(error) => {
                    diagnostics.push(format!("permissions: skipped {} ({error})", path.display()))
                }
            }
        }
    }

    let repo_path = workspace_permissions_path(workspace);
    if let Ok(content) = std::fs::read_to_string(&repo_path) {
        match parse_permissions_toml(&content) {
            Ok(file) => {
                if !file.allow.is_empty() {
                    diagnostics.push(format!(
                        "permissions: ignored {} `allow` rule(s) from {} — repo-committed \
                         rules can only narrow approval (deny), not skip it; add `allow` \
                         rules to your personal permissions file instead",
                        file.allow.len(),
                        repo_path.display()
                    ));
                }
                merged.deny.extend(file.deny);
            }
            Err(error) => diagnostics.push(format!(
                "permissions: skipped {} ({error})",
                repo_path.display()
            )),
        }
    }

    (merged, diagnostics)
}

/// Append an `allow` pattern to the personal permissions file, creating it
/// (and its parent directory) if it doesn't exist yet. Used by the "always
/// allow this pattern going forward" approval decision. A no-op if the
/// pattern is already present.
pub fn append_user_allow_rule(pattern: &str) -> std::io::Result<()> {
    let path = user_permissions_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no platform config directory available",
        )
    })?;
    append_allow_rule(&path, pattern)
}

fn append_allow_rule(path: &Path, pattern: &str) -> std::io::Result<()> {
    let mut file = match std::fs::read_to_string(path) {
        Ok(content) => parse_permissions_toml(&content).unwrap_or_default(),
        Err(_) => PermissionsFile::default(),
    };
    if file.allow.iter().any(|existing| existing == pattern) {
        return Ok(());
    }
    file.allow.push(pattern.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized =
        toml::to_string_pretty(&file).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, serialized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn parses_allow_and_deny_arrays() {
        let file = parse_permissions_toml(
            "allow = [\"bash(cargo test *)\"]\ndeny = [\"bash(cargo publish*)\"]\n",
        )
        .unwrap();
        assert_eq!(file.allow, vec!["bash(cargo test *)".to_string()]);
        assert_eq!(file.deny, vec!["bash(cargo publish*)".to_string()]);
    }

    #[test]
    fn missing_arrays_default_to_empty() {
        let file = parse_permissions_toml("").unwrap();
        assert!(file.allow.is_empty());
        assert!(file.deny.is_empty());
    }

    #[test]
    fn repo_allow_entries_are_ignored_but_deny_entries_are_honored() {
        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("permissions.toml"),
            "allow = [\"bash(*)\"]\ndeny = [\"bash(rm -rf*)\"]\n",
        )
        .unwrap();

        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(
            merged.allow.is_empty(),
            "repo-scope allow rules must never be honored: {:?}",
            merged.allow
        );
        assert_eq!(merged.deny, vec!["bash(rm -rf*)".to_string()]);
        assert!(
            diagnostics.iter().any(|d| d.contains("ignored")),
            "a diagnostic should explain why the repo allow rule was dropped: {diagnostics:?}"
        );
    }

    #[test]
    fn missing_files_merge_to_empty_without_diagnostics() {
        let dir = TempDir::new().unwrap();
        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(merged.allow.is_empty());
        assert!(merged.deny.is_empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn malformed_repo_file_is_skipped_with_a_diagnostic() {
        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(forge_dir.join("permissions.toml"), "not valid toml [[[").unwrap();

        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(merged.allow.is_empty());
        assert!(merged.deny.is_empty());
        assert!(diagnostics.iter().any(|d| d.contains("skipped")));
    }

    #[test]
    fn append_allow_rule_creates_file_and_dedupes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.toml");

        append_allow_rule(&path, "bash(cargo test *)").unwrap();
        append_allow_rule(&path, "bash(cargo test *)").unwrap();
        append_allow_rule(&path, "bash(cargo build*)").unwrap();

        let file = parse_permissions_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            file.allow,
            vec![
                "bash(cargo test *)".to_string(),
                "bash(cargo build*)".to_string()
            ]
        );
    }

    #[test]
    fn append_allow_rule_preserves_existing_deny_entries() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.toml");
        std::fs::write(&path, "deny = [\"bash(rm -rf*)\"]\n").unwrap();

        append_allow_rule(&path, "bash(cargo test *)").unwrap();

        let file = parse_permissions_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.deny, vec!["bash(rm -rf*)".to_string()]);
        assert_eq!(file.allow, vec!["bash(cargo test *)".to_string()]);
    }
}
