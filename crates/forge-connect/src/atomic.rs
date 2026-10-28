//! Atomic replacement of forge-connect's config files.
//!
//! A crash or a concurrent reader never sees a half-written file: the bytes go
//! to a temporary file in the same directory, are synced, and the temporary is
//! renamed over the target. A symlinked target is resolved first so the link
//! survives and its destination is what changes.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NONCE: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` atomically. `mode` sets Unix permissions on the new
/// file before any byte is written (`0o600` for files that hold secrets).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> std::io::Result<()> {
    write_atomic_checked(path, bytes, mode, || Ok(()))
}

/// As [`write_atomic`], running `before_rename` after the bytes are synced and
/// before the rename. An error from it aborts the write and leaves `path`
/// untouched (used for "the file changed while saving" checks).
pub(crate) fn write_atomic_checked(
    path: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    before_rename: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let target = resolve_symlink(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let temporary = target.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        #[cfg(not(unix))]
        let _ = mode;
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        before_rename()?;
        std::fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn resolve_symlink(path: &Path) -> PathBuf {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_contents_and_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        std::fs::write(&path, "old").unwrap();
        write_atomic(&path, b"new", None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_check_leaves_the_target_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.toml");
        std::fs::write(&path, "old").unwrap();
        let error = write_atomic_checked(&path, b"new", None, || {
            Err(std::io::Error::other("changed"))
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "changed");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn secrets_are_created_private_and_symlinks_survive() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "old").unwrap();
        let link = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        write_atomic(&link, b"secret", Some(0o600)).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "secret");
        let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
