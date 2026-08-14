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
        persist_committed_theme_at(&path, "solarized-light").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("keep-me"));
        assert!(text.contains("theme_committed"));
    }
}
