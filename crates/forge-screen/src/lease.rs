//! One controller of the screen at a time.
//!
//! There is one mouse and one keyboard. A session that starts using them
//! holds the lease until it stops (Stop, a takeover, a cancelled turn, the
//! session ending); any other session is refused meanwhile. Inside one
//! Forge process the lease is a mutex; across processes (the app and a
//! `forge bench` run) an `flock` on a file in the runtime directory.
//! Releasing the lease revokes the holder's screen-control consent.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use forge_tools::SessionConsents;
use forge_types::{ConsentKind, SessionId};

/// Who holds the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    pub session: SessionId,
}

/// Why the lease was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseRefused {
    /// Another session of this process holds it.
    Session(SessionId),
    /// Another Forge process holds it.
    OtherProcess,
    /// The lock file could not be used.
    Io(String),
}

impl std::fmt::Display for LeaseRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseRefused::Session(_) => {
                f.write_str("another session is controlling the screen; it has to stop first")
            }
            LeaseRefused::OtherProcess => {
                f.write_str("another Forge process is controlling the screen; it has to stop first")
            }
            LeaseRefused::Io(error) => write!(f, "could not take the screen: {error}"),
        }
    }
}

pub struct ScreenController {
    lock_path: PathBuf,
    held: Mutex<Option<Held>>,
}

struct Held {
    session: SessionId,
    consents: Arc<SessionConsents>,
    /// Holds the `flock`; closing it releases the lock.
    _file: std::fs::File,
}

impl ScreenController {
    /// A controller whose cross-process lock is `lock_path`.
    pub fn new(lock_path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            lock_path,
            held: Mutex::new(None),
        })
    }

    /// The process-wide controller, locking
    /// `$XDG_RUNTIME_DIR/forge-screen.lock` (or the temp dir).
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<ScreenController>> = OnceLock::new();
        GLOBAL
            .get_or_init(|| {
                ScreenController::new(default_lock_path(
                    std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
                ))
            })
            .clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Held>> {
        self.held
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Take the screen for `session`. Idempotent for the holder. `consents`
    /// is the holder's store, whose screen-control grant is revoked when
    /// the lease is released.
    pub fn acquire(
        &self,
        session: SessionId,
        consents: Arc<SessionConsents>,
    ) -> Result<(), LeaseRefused> {
        let mut held = self.lock();
        if let Some(current) = held.as_ref() {
            return if current.session == session {
                Ok(())
            } else {
                Err(LeaseRefused::Session(current.session))
            };
        }
        let file =
            open_lock(&self.lock_path).map_err(|error| LeaseRefused::Io(error.to_string()))?;
        // SAFETY: flock on a descriptor this function owns.
        let locked = unsafe {
            libc::flock(
                std::os::fd::AsRawFd::as_raw_fd(&file),
                libc::LOCK_EX | libc::LOCK_NB,
            )
        };
        if locked != 0 {
            let error = std::io::Error::last_os_error();
            return Err(if error.kind() == std::io::ErrorKind::WouldBlock {
                LeaseRefused::OtherProcess
            } else {
                LeaseRefused::Io(error.to_string())
            });
        }
        *held = Some(Held {
            session,
            consents,
            _file: file,
        });
        Ok(())
    }

    /// Give the screen back and revoke the session's screen-control
    /// consent. `false` when `session` did not hold it.
    pub fn release(&self, session: SessionId) -> bool {
        let mut held = self.lock();
        match held.as_ref() {
            Some(current) if current.session == session => {
                current.consents.revoke(ConsentKind::ScreenControl);
                *held = None;
                true
            }
            _ => false,
        }
    }

    pub fn holder(&self) -> Option<Holder> {
        self.lock().as_ref().map(|held| Holder {
            session: held.session,
        })
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

/// Where the cross-process lock lives: the user's private runtime dir, or
/// a per-user name in the shared temp dir.
pub fn default_lock_path(runtime_dir: Option<PathBuf>) -> PathBuf {
    match runtime_dir.filter(|dir| dir.is_absolute() && dir.is_dir()) {
        Some(dir) => dir.join("forge-screen.lock"),
        None => {
            // SAFETY: getuid has no preconditions.
            let uid = unsafe { libc::getuid() };
            std::env::temp_dir().join(format!("forge-screen-{uid}.lock"))
        }
    }
}

/// The lock file, private to the user.
fn open_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_types::ConsentKind;

    fn session(n: u128) -> SessionId {
        SessionId::from_u128(n)
    }

    #[test]
    fn one_session_holds_the_screen_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        let controller = ScreenController::new(dir.path().join("screen.lock"));
        let consents = SessionConsents::new();
        assert_eq!(controller.acquire(session(1), consents.clone()), Ok(()));
        assert_eq!(
            controller.acquire(session(1), consents.clone()),
            Ok(()),
            "idempotent"
        );
        assert_eq!(
            controller.acquire(session(2), SessionConsents::new()),
            Err(LeaseRefused::Session(session(1)))
        );
        assert_eq!(
            controller.holder(),
            Some(Holder {
                session: session(1)
            })
        );
        assert!(!controller.release(session(2)), "only the holder releases");
        assert!(controller.release(session(1)));
        assert_eq!(controller.holder(), None);
        assert_eq!(
            controller.acquire(session(2), SessionConsents::new()),
            Ok(())
        );
    }

    #[test]
    fn releasing_revokes_the_holders_screen_consent() {
        let dir = tempfile::tempdir().unwrap();
        let controller = ScreenController::new(dir.path().join("screen.lock"));
        let consents = SessionConsents::new();
        consents.grant(ConsentKind::ScreenControl, "screen");
        consents.grant(ConsentKind::FormSubmit, "example.com");
        controller.acquire(session(1), consents.clone()).unwrap();
        controller.release(session(1));
        assert!(!consents.is_granted(ConsentKind::ScreenControl, "screen"));
        assert!(consents.is_granted(ConsentKind::FormSubmit, "example.com"));
    }

    /// Two controllers on one lock file stand in for two Forge processes.
    #[test]
    fn another_process_holding_the_lock_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("screen.lock");
        let app = ScreenController::new(path.clone());
        let bench = ScreenController::new(path);
        app.acquire(session(1), SessionConsents::new()).unwrap();
        assert_eq!(
            bench.acquire(session(2), SessionConsents::new()),
            Err(LeaseRefused::OtherProcess)
        );
        app.release(session(1));
        assert_eq!(bench.acquire(session(2), SessionConsents::new()), Ok(()));
    }

    #[test]
    fn refusals_read_as_tool_messages() {
        assert!(LeaseRefused::Session(session(7))
            .to_string()
            .contains("another session"));
        assert!(LeaseRefused::OtherProcess
            .to_string()
            .contains("another Forge"));
    }

    /// Outside a private runtime dir the lock is per user: another user
    /// cannot hold it (or block it) for everyone.
    #[test]
    fn the_lock_is_private_to_the_user() {
        let runtime = tempfile::tempdir().unwrap();
        assert_eq!(
            default_lock_path(Some(runtime.path().to_path_buf())),
            runtime.path().join("forge-screen.lock")
        );
        // SAFETY: getuid has no preconditions.
        let uid = unsafe { libc::getuid() };
        assert_eq!(
            default_lock_path(None),
            std::env::temp_dir().join(format!("forge-screen-{uid}.lock"))
        );
    }

    #[test]
    fn the_global_controller_is_shared() {
        assert!(Arc::ptr_eq(
            &ScreenController::global(),
            &ScreenController::global()
        ));
        let name = ScreenController::global()
            .lock_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("forge-screen") && name.ends_with(".lock"),
            "{name}"
        );
    }
}
