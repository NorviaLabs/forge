//! Directory trust (user-global).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    // A malformed file is an error, never an empty list: `grant_trust` saves
    // what it loaded, so an empty fallback would erase every other entry.
    // Readers (`is_trusted_at`) treat the error as untrusted.
    Ok(toml::from_str(&text)?)
}

/// Replace the file atomically: a crash mid-write must not leave a
/// truncated file, which reads as "nothing is trusted".
fn save_at(path: &Path, file: &TrustFile) -> Result<(), TrustError> {
    use std::io::Write;
    let text = toml::to_string_pretty(file)?;
    let temporary = path.with_extension(format!("toml.tmp-{}", std::process::id()));
    let mut out = fs::File::create(&temporary)?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
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
    let dir = store.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    // Read, change and replace under a lock, so two grants at once (two
    // Forge windows, or a project and one of its worktrees) both land.
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("trust.lock"))?;
    lock.lock()?;
    let mut file = load_at(store)?;
    let key = canonical.display().to_string();
    if !file.paths.iter().any(|p| p == &key) {
        file.paths.push(key);
        save_at(store, &file)?;
    }
    drop(lock);
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Grants racing from several threads must all land: each one reads,
    /// adds its path and replaces the whole file.
    #[test]
    fn concurrent_grants_all_land() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("config").join("trust.toml");
        let folders: Vec<PathBuf> = (0..8)
            .map(|index| {
                let folder = dir.path().join(format!("project-{index}"));
                fs::create_dir_all(&folder).unwrap();
                folder
            })
            .collect();

        std::thread::scope(|scope| {
            for folder in &folders {
                let store = &store;
                scope.spawn(move || grant_trust_at(store, folder).unwrap());
            }
        });

        for folder in &folders {
            assert!(is_trusted_at(&store, folder), "{} lost", folder.display());
        }
    }

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

    /// A malformed trust file makes `grant_trust` fail instead of replacing
    /// the file with only the new path; readers treat it as untrusted.
    #[test]
    fn grant_trust_refuses_a_malformed_trust_file() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("trust.toml");
        let workspace = dir.path().join("ws");
        fs::create_dir_all(&workspace).unwrap();
        let original = "paths = [\"/kept\"\n";
        fs::write(&store, original).unwrap();
        assert!(grant_trust_at(&store, &workspace).is_err());
        assert_eq!(fs::read_to_string(&store).unwrap(), original);
        assert!(!is_trusted_at(&store, &workspace));
    }

    /// Granting creates the store's parent directory rather than failing when the
    /// config dir does not exist yet (first run).
    #[test]
    fn granting_creates_missing_parent_directories() {
        let dir = TempDir::new().unwrap();
        let store = dir.path().join("nested").join("deeper").join("trust.toml");
        grant_trust_at(&store, dir.path()).unwrap();
        assert!(store.is_file());
    }
}
