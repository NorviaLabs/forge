//! Directory trust and committed-theme persistence (user-global).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{normalize_theme_id, user_config_path, ConfigError};

/// Directory name under `$HOME` called out on the trust screen.
pub const HOME_PROJECTS_DIR: &str = "Projects";

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("toml write error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("cannot canonicalize working directory")]
    Canonicalize,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    paths: Vec<String>,
}

pub fn trust_file_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("forge").join("trust.toml"))
}

fn load_at(path: &Path) -> Result<TrustFile, TrustError> {
    if !path.is_file() {
        return Ok(TrustFile::default());
    }
    let text = fs::read_to_string(path)?;
    Ok(toml::from_str(&text).unwrap_or_default())
}

fn save_at(path: &Path, file: &TrustFile) -> Result<(), TrustError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(file)?)?;
    Ok(())
}

/// Path shown on the trust screen: canonical when possible, otherwise raw.
pub fn trust_display_path(cwd: &Path) -> String {
    cwd.canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .display()
        .to_string()
}

/// True when `cwd` or an ancestor is listed in the user-global trust file.
pub fn is_trusted(cwd: &Path) -> bool {
    match trust_file_path() {
        Some(path) => is_trusted_at(&path, cwd),
        None => false,
    }
}

pub fn is_trusted_at(store: &Path, cwd: &Path) -> bool {
    let Ok(canonical) = cwd.canonicalize() else {
        return false;
    };
    let Ok(file) = load_at(store) else {
        return false;
    };
    let trusted: Vec<PathBuf> = file.paths.into_iter().map(PathBuf::from).collect();
    let mut cursor = canonical;
    loop {
        if trusted.iter().any(|p| p == &cursor) {
            return true;
        }
        if !cursor.pop() {
            return false;
        }
    }
}

/// Persist trust for the canonical cwd only. Fails if the path cannot be resolved.
pub fn grant_trust(cwd: &Path) -> Result<PathBuf, TrustError> {
    let store = trust_file_path().ok_or(TrustError::Canonicalize)?;
    grant_trust_at(&store, cwd)
}

pub fn grant_trust_at(store: &Path, cwd: &Path) -> Result<PathBuf, TrustError> {
    let canonical = cwd.canonicalize().map_err(|_| TrustError::Canonicalize)?;
    let mut file = load_at(store)?;
    let key = canonical.display().to_string();
    if !file.paths.iter().any(|p| p == &key) {
        file.paths.push(key);
        save_at(store, &file)?;
    }
    Ok(canonical)
}

/// Write `[tui] theme` and `theme_committed = true` into the user config file.
pub fn persist_committed_theme(theme_id: &str) -> Result<(), ConfigError> {
    let path = user_config_path()
        .ok_or_else(|| ConfigError::Message("no user config directory is available".into()))?;
    persist_committed_theme_at(&path, theme_id)
}

pub fn persist_committed_theme_at(path: &Path, theme_id: &str) -> Result<(), ConfigError> {
    let id = normalize_theme_id(theme_id);
    let mut value: toml::Value = if path.is_file() {
        let text = fs::read_to_string(path)?;
        toml::from_str(&text).unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()))
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = value
        .as_table_mut()
        .ok_or_else(|| ConfigError::Message("user config is not a TOML table".into()))?;
    let tui = table
        .entry("tui")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let tui = tui
        .as_table_mut()
        .ok_or_else(|| ConfigError::Message("[tui] is not a table".into()))?;
    tui.insert("theme".into(), toml::Value::String(id));
    tui.insert("theme_committed".into(), toml::Value::Boolean(true));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        toml::to_string_pretty(&value).map_err(|e| ConfigError::Message(e.to_string()))?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_store_is_untrusted() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let cwd = dir.path();
        assert!(!is_trusted_at(&store, cwd));
    }

    #[test]
    fn grant_trusts_cwd_and_children() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let child = dir.path().join("crates").join("tui");
        fs::create_dir_all(&child).unwrap();
        grant_trust_at(&store, dir.path()).unwrap();
        assert!(is_trusted_at(&store, dir.path()));
        assert!(is_trusted_at(&store, &child));
    }

    #[test]
    fn sibling_is_not_trusted() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let a = dir.path().join("forge");
        let b = dir.path().join("forge-feat");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        grant_trust_at(&store, &a).unwrap();
        assert!(is_trusted_at(&store, &a));
        assert!(!is_trusted_at(&store, &b));
    }

    #[test]
    fn persist_theme_writes_committed_flag() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        persist_committed_theme_at(&path, "forge-dark").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("theme_committed"));
        assert!(text.contains("forge-dark"));
    }

    #[test]
    fn persist_theme_preserves_other_keys() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[model]\nmodel = \"keep-me\"\n").unwrap();
        persist_committed_theme_at(&path, "forge-light").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("keep-me"));
        assert!(text.contains("theme_committed"));
    }

    /// A directory that cannot be canonicalized (it does not exist) is never
    /// trusted, and cannot be granted trust either — trust is only ever
    /// recorded against a resolved path.
    #[test]
    fn nonexistent_cwd_is_untrusted_and_cannot_be_granted() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let missing = dir.path().join("does-not-exist");
        grant_trust_at(&store, dir.path()).unwrap();
        // Even though the parent is trusted, an unresolvable path is not.
        assert!(!is_trusted_at(&store, &missing));
        assert!(matches!(
            grant_trust_at(&store, &missing),
            Err(TrustError::Canonicalize)
        ));
    }

    /// A corrupt trust file fails closed (untrusted) instead of erroring or
    /// being treated as trusting everything.
    #[test]
    fn corrupt_store_fails_closed() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        fs::write(&store, "this is not [[[ toml").unwrap();
        assert!(!is_trusted_at(&store, dir.path()));

        // And a well-formed file with the wrong shape is equally inert.
        fs::write(&store, "paths = \"not-a-list\"\n").unwrap();
        assert!(!is_trusted_at(&store, dir.path()));
    }

    /// An empty `paths` list trusts nothing; the walk to the filesystem root
    /// terminates rather than looping.
    #[test]
    fn empty_paths_list_trusts_nothing() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        fs::write(&store, "paths = []\n").unwrap();
        assert!(!is_trusted_at(&store, dir.path()));
        let deep = dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        assert!(!is_trusted_at(&store, &deep));
    }

    /// Granting twice must not duplicate the entry, and the returned path is
    /// the canonical one (which on macOS differs from the raw temp path).
    #[test]
    fn grant_is_idempotent_and_stores_the_canonical_path() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let first = grant_trust_at(&store, dir.path()).unwrap();
        let second = grant_trust_at(&store, dir.path()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first, dir.path().canonicalize().unwrap());

        let file: TrustFile = toml::from_str(&fs::read_to_string(&store).unwrap()).unwrap();
        assert_eq!(file.paths, vec![first.display().to_string()]);
    }

    /// Two different directories both persist, and neither grants the other.
    #[test]
    fn multiple_grants_accumulate_independently() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        grant_trust_at(&store, &a).unwrap();
        grant_trust_at(&store, &b).unwrap();

        let file: TrustFile = toml::from_str(&fs::read_to_string(&store).unwrap()).unwrap();
        assert_eq!(file.paths.len(), 2);
        assert!(is_trusted_at(&store, &a));
        assert!(is_trusted_at(&store, &b));
        // A prefix-sharing sibling name is not trusted by string prefix.
        let a_sibling = dir.path().join("ab");
        fs::create_dir_all(&a_sibling).unwrap();
        assert!(!is_trusted_at(&store, &a_sibling));
    }

    /// Trust is recorded against the resolved target, so reaching the same
    /// directory through a symlink is still trusted — and, conversely, granting
    /// through a symlink trusts the real directory.
    #[cfg(unix)]
    #[test]
    fn trust_follows_symlinks_to_the_real_directory() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let real = dir.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        grant_trust_at(&store, &link).unwrap();
        assert!(is_trusted_at(&store, &real));
        assert!(is_trusted_at(&store, &link));
    }

    /// The trust screen shows the resolved path when it can, and falls back to
    /// the raw path rather than failing when it cannot.
    #[test]
    fn display_path_falls_back_to_the_raw_path() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            trust_display_path(dir.path()),
            dir.path().canonicalize().unwrap().display().to_string()
        );
        let missing = dir.path().join("nope");
        assert_eq!(trust_display_path(&missing), missing.display().to_string());
    }

    /// The persisted theme id goes through `normalize_theme_id`, so legacy
    /// aliases are stored as their canonical id rather than round-tripping as
    /// `dark`/`light`.
    #[test]
    fn persist_theme_normalizes_legacy_aliases() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        persist_committed_theme_at(&path, "  DARK  ").unwrap();
        let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["tui"]["theme"].as_str(), Some("forge-dark"));
        assert_eq!(value["tui"]["theme_committed"].as_bool(), Some(true));

        // An empty id resolves to the default rather than being written blank.
        persist_committed_theme_at(&path, "").unwrap();
        let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["tui"]["theme"].as_str(),
            Some(crate::DEFAULT_THEME_ID)
        );
    }

    /// Committing a second theme overwrites the first and leaves unrelated
    /// `[tui]` keys alone.
    #[test]
    fn persist_theme_overwrites_and_keeps_sibling_tui_keys() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[tui]\ntheme = \"forge-light\"\nmouse = true\n").unwrap();
        persist_committed_theme_at(&path, "forge-dark").unwrap();
        let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["tui"]["theme"].as_str(), Some("forge-dark"));
        assert_eq!(value["tui"]["mouse"].as_bool(), Some(true));
        assert_eq!(value["tui"]["theme_committed"].as_bool(), Some(true));
    }

    /// An unparseable user config is replaced rather than aborting the commit —
    /// the theme choice the user just made still lands.
    #[test]
    fn persist_theme_replaces_an_unparseable_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "not [[[ toml").unwrap();
        persist_committed_theme_at(&path, "forge-dark").unwrap();
        let value: toml::Value = toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["tui"]["theme"].as_str(), Some("forge-dark"));
    }

    /// `[tui]` present as a non-table is a config error, not a panic.
    #[test]
    fn persist_theme_rejects_a_non_table_tui_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "tui = \"oops\"\n").unwrap();
        assert!(persist_committed_theme_at(&path, "forge-dark").is_err());
    }

    /// Both writers create their parent directory rather than failing when the
    /// config dir does not exist yet (first run).
    #[test]
    fn writers_create_missing_parent_directories() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("nested").join("deeper").join("trust.toml");
        grant_trust_at(&store, dir.path()).unwrap();
        assert!(store.is_file());

        let config = dir.path().join("other").join("config.toml");
        persist_committed_theme_at(&config, "forge-dark").unwrap();
        assert!(config.is_file());
    }
}
