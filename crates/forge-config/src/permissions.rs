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
//! strings. `host(...)` rules are an exception at the consumer: they are
//! applied to the egress proxy rather than the HITL gate.

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

    /// Points the platform config dir (via `HOME`/`XDG_CONFIG_HOME`) at an
    /// empty temp dir for the duration of the test, so `load_permissions`'s
    /// user scope never reads the developer's real `permissions.toml`. Shares
    /// the crate-wide `ENV_LOCK` with `lib.rs`'s env guards (rustc runs tests
    /// in parallel threads), and restores the environment on drop — including
    /// on assertion failure.
    struct IsolatedUserConfig {
        _lock: std::sync::MutexGuard<'static, ()>,
        _home: TempDir,
        saved: Vec<(String, Option<String>)>,
    }

    impl IsolatedUserConfig {
        fn new() -> Self {
            let _lock = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let mut saved = Vec::new();
            for key in ["HOME", "XDG_CONFIG_HOME"] {
                saved.push((key.to_string(), std::env::var(key).ok()));
            }
            let home = TempDir::new().unwrap();
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::set_var("HOME", home.path());
            Self {
                _lock,
                _home: home,
                saved,
            }
        }

        /// The user-scope file path under the redirected config dir, so a test
        /// can seed a real personal file and assert it is loaded.
        fn user_permissions_path(&self) -> PathBuf {
            user_permissions_path().expect("config dir resolves under redirected HOME")
        }
    }

    impl Drop for IsolatedUserConfig {
        fn drop(&mut self) {
            for (key, val) in self.saved.drain(..) {
                match val {
                    Some(v) => std::env::set_var(&key, v),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }

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
        let _user = IsolatedUserConfig::new();
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
        let _user = IsolatedUserConfig::new();
        let dir = TempDir::new().unwrap();
        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(merged.allow.is_empty());
        assert!(merged.deny.is_empty());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn malformed_repo_file_is_skipped_with_a_diagnostic() {
        let _user = IsolatedUserConfig::new();
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
    fn user_scope_file_is_still_loaded_under_isolation() {
        let user = IsolatedUserConfig::new();
        let path = user.user_permissions_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "allow = [\"bash(cargo test *)\"]\n").unwrap();

        let dir = TempDir::new().unwrap();
        let (merged, diagnostics) = load_permissions(dir.path());
        assert_eq!(merged.allow, vec!["bash(cargo test *)".to_string()]);
        assert!(merged.deny.is_empty());
        assert!(diagnostics.is_empty());
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

    /// Seeds both scopes and returns the workspace dir. Keeps the guard alive
    /// in the caller so `HOME` stays redirected for the whole test.
    fn seed_both_scopes(user: &IsolatedUserConfig, user_toml: &str, repo_toml: &str) -> TempDir {
        let path = user.user_permissions_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, user_toml).unwrap();

        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(forge_dir.join("permissions.toml"), repo_toml).unwrap();
        dir
    }

    /// The merge is personal-first: personal `allow` survives, personal `deny`
    /// and repo `deny` are concatenated in scope order.
    #[test]
    fn both_scopes_merge_with_personal_allow_and_all_denies() {
        let user = IsolatedUserConfig::new();
        let dir = seed_both_scopes(
            &user,
            "allow = [\"bash(cargo test *)\"]\ndeny = [\"bash(curl*)\"]\n",
            "deny = [\"bash(rm -rf*)\"]\n",
        );

        let (merged, diagnostics) = load_permissions(dir.path());
        assert_eq!(merged.allow, vec!["bash(cargo test *)".to_string()]);
        assert_eq!(
            merged.deny,
            vec!["bash(curl*)".to_string(), "bash(rm -rf*)".to_string()]
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// A repo file with only `deny` rules is fully trusted and produces no
    /// "ignored" diagnostic — the warning is specifically about `allow`.
    #[test]
    fn deny_only_repo_file_produces_no_diagnostic() {
        let _user = IsolatedUserConfig::new();
        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("permissions.toml"),
            "deny = [\"bash(rm -rf*)\", \"write_file(/etc/*)\"]\n",
        )
        .unwrap();

        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(merged.allow.is_empty());
        assert_eq!(merged.deny.len(), 2);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    /// The "ignored" diagnostic names the file and the number of dropped rules
    /// so the user can find and move them.
    #[test]
    fn ignored_allow_diagnostic_names_the_file_and_the_count() {
        let _user = IsolatedUserConfig::new();
        let dir = TempDir::new().unwrap();
        let forge_dir = dir.path().join(".forge");
        std::fs::create_dir_all(&forge_dir).unwrap();
        std::fs::write(
            forge_dir.join("permissions.toml"),
            "allow = [\"bash(*)\", \"write_file(*)\", \"git(push*)\"]\n",
        )
        .unwrap();

        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(merged.allow.is_empty());
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains('3'), "{}", diagnostics[0]);
        assert!(
            diagnostics[0].contains(&workspace_permissions_path(dir.path()).display().to_string()),
            "{}",
            diagnostics[0]
        );
    }

    /// A malformed *personal* file is skipped with its own diagnostic, and does
    /// not stop the repo file from being read.
    #[test]
    fn malformed_user_file_is_skipped_without_blocking_the_repo_file() {
        let user = IsolatedUserConfig::new();
        let dir = seed_both_scopes(&user, "allow = not-valid-toml", "deny = [\"bash(rm*)\"]\n");

        let (merged, diagnostics) = load_permissions(dir.path());
        assert!(merged.allow.is_empty());
        assert_eq!(merged.deny, vec!["bash(rm*)".to_string()]);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].starts_with("permissions: skipped"));
        assert!(
            diagnostics[0].contains(&user.user_permissions_path().display().to_string()),
            "{}",
            diagnostics[0]
        );
    }

    /// A repo `.forge` directory that exists but has no permissions file is the
    /// common case and must be silent.
    #[test]
    fn workspace_without_a_permissions_file_is_silent() {
        let _user = IsolatedUserConfig::new();
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join(".forge")).unwrap();
        let (merged, diagnostics) = load_permissions(dir.path());
        assert_eq!(merged, PermissionsFile::default());
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn permissions_paths_have_the_documented_shape() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            workspace_permissions_path(dir.path()),
            dir.path().join(".forge").join("permissions.toml")
        );
        let _user = IsolatedUserConfig::new();
        let user_path = user_permissions_path().unwrap();
        assert!(
            user_path.ends_with("forge/permissions.toml"),
            "{user_path:?}"
        );
    }

    /// Unknown keys are tolerated (forward compatibility) but a wrongly typed
    /// `allow`/`deny` is a parse error, not a silent empty list.
    #[test]
    fn parse_tolerates_unknown_keys_but_rejects_wrong_types() {
        let file = parse_permissions_toml("allow = [\"bash(ls)\"]\nfuture_key = 1\n").unwrap();
        assert_eq!(file.allow, vec!["bash(ls)".to_string()]);

        assert!(parse_permissions_toml("allow = \"bash(ls)\"\n").is_err());
        assert!(parse_permissions_toml("deny = [1, 2]\n").is_err());
        assert!(parse_permissions_toml("not valid [[[").is_err());
    }

    /// Empty arrays are dropped on write, so appending one allow rule to a
    /// fresh file does not leave a stray `deny = []` behind.
    #[test]
    fn empty_arrays_are_not_serialized() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.toml");
        append_allow_rule(&path, "bash(cargo test *)").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("deny"), "{text}");
    }

    /// The personal file is created along with its parent directory on first
    /// use — an "always allow" decision must not fail on a fresh machine.
    #[test]
    fn append_allow_rule_creates_missing_parent_directories() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("forge").join("permissions.toml");
        append_allow_rule(&path, "bash(cargo test *)").unwrap();
        assert!(path.is_file());
        let file = parse_permissions_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.allow, vec!["bash(cargo test *)".to_string()]);
    }

    /// Documented sharp edge: an existing personal file that does not parse is
    /// treated as absent, so appending a rule REPLACES it. Nothing that was in
    /// the broken file (including its `deny` entries) survives. It fails toward
    /// an empty ruleset rather than toward a wider one, so this is safe, but it
    /// is lossy and worth pinning.
    #[test]
    fn append_allow_rule_discards_an_unparseable_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("permissions.toml");
        std::fs::write(&path, "deny = [\"bash(rm -rf*)\"]\nbroken [[[\n").unwrap();

        append_allow_rule(&path, "bash(cargo test *)").unwrap();

        let file = parse_permissions_toml(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.allow, vec!["bash(cargo test *)".to_string()]);
        assert!(
            file.deny.is_empty(),
            "the unparseable file's deny rules do not survive: {:?}",
            file.deny
        );
    }

    /// Round trip: a file written by `append_allow_rule` is loadable by
    /// `load_permissions` through the real personal-scope path.
    #[test]
    fn appended_rule_is_visible_to_the_next_load() {
        let user = IsolatedUserConfig::new();
        append_allow_rule(&user.user_permissions_path(), "bash(cargo test *)").unwrap();
        let dir = TempDir::new().unwrap();
        let (merged, diagnostics) = load_permissions(dir.path());
        assert_eq!(merged.allow, vec!["bash(cargo test *)".to_string()]);
        assert!(diagnostics.is_empty());
    }
}
