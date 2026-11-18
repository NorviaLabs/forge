//! Where the computer-use tools keep the screenshots they take.
//!
//! Not the session's temp directory: every sandboxed command can write
//! there, so a command could swap the folder for a link and have Forge
//! write files wherever the user can. The store is a private directory of
//! Forge's own that no sandbox sees, and each file is created new — never
//! through a link, never over another file. The session copies each
//! attachment into its image cache as soon as the tool returns, so the
//! store only has to outlive the call.

use std::path::PathBuf;
use std::sync::Mutex;

/// A private directory for one manager's screenshots, made on first use.
#[derive(Debug, Default)]
pub struct ScreenshotStore {
    dir: Mutex<Option<tempfile::TempDir>>,
}

impl ScreenshotStore {
    /// Save `bytes` as a new `<prefix>-<unique>.<ext>` and return its path.
    pub fn save(&self, prefix: &str, ext: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let dir = {
            let mut dir = self
                .dir
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if dir.is_none() {
                // A fresh directory only the user can enter.
                let mut builder = tempfile::Builder::new();
                builder.prefix("forge-shots-");
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    builder.permissions(std::fs::Permissions::from_mode(0o700));
                }
                *dir = Some(builder.tempdir()?);
            }
            dir.as_ref().expect("just made").path().to_path_buf()
        };
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0);
        let path = dir.join(format!(
            "{prefix}-{nanos}-{}.{ext}",
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        write_new(&path, bytes)?;
        Ok(path)
    }
}

/// Write `bytes` to `path`, which must not exist yet (a link there counts).
pub fn write_new(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // O_CREAT|O_EXCL: refuses an existing file, and a link, without
    // following it.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screenshot_lands_in_a_private_directory() {
        use std::os::unix::fs::PermissionsExt;
        let store = ScreenshotStore::default();
        let first = store.save("web-check", "png", b"one").unwrap();
        let second = store.save("web-check", "png", b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        let name = first.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with("web-check-") && name.ends_with(".png"),
            "{name}"
        );
        let dir = first.parent().unwrap();
        assert_eq!(second.parent().unwrap(), dir);
        let mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "only the user can look in");
    }

    /// A file is never written through a link or over another file.
    #[test]
    fn a_link_or_a_file_in_the_way_is_not_written_through() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.txt");
        std::fs::write(&outside, "keep").unwrap();
        let link = dir.path().join("shot.png");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(write_new(&link, b"png").is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        assert!(write_new(&outside, b"png").is_err());
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
        let fresh = dir.path().join("fresh.png");
        write_new(&fresh, b"png").unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"png");
    }
}
