//! What remote devices did, one JSON object per line in `activity.jsonl`.
//!
//! Approvals are also journaled in the session with `actor:
//! remote:<device>`; this log covers what the journal does not attribute:
//! pairing, revoking, prompts and stops. It never holds a prompt's text (the
//! journal has it) or a token.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use forge_types::SessionId;
use serde::Serialize;

use crate::clock::Clock;
use crate::fsutil::append_private;
use std::io::Write;

pub const ACTIVITY_FILE: &str = "activity.jsonl";
/// The log rotates to `activity.jsonl.1` past this size.
pub const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Activity {
    Enabled {
        address: String,
    },
    Disabled,
    Paired {
        device: String,
    },
    /// A device redeemed the code and waits for the desktop.
    PairingRequested {
        device: String,
    },
    /// The desktop refused a device's pairing request.
    PairingDeclined {
        device: String,
    },
    Revoked {
        device: String,
    },
    Prompt {
        device: String,
        session_id: SessionId,
        /// The prompt's length in characters; never its text.
        chars: usize,
    },
    Decision {
        device: String,
        session_id: SessionId,
        call_id: String,
        decision: String,
    },
    Stop {
        device: String,
        session_id: SessionId,
    },
    /// Requests refused for a missing or wrong token since the last entry
    /// of this kind.
    AuthFailures {
        count: u64,
    },
    /// Pairing attempts refused (wrong, expired or burnt code) since the
    /// last entry of this kind.
    PairingRefused {
        count: u64,
    },
}

pub struct ActivityLog {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    max_bytes: u64,
    /// One writer at a time, so lines never interleave and rotation is
    /// not raced.
    lock: Mutex<()>,
}

impl ActivityLog {
    pub fn new(dir: &Path, clock: Arc<dyn Clock>) -> Self {
        Self {
            path: dir.join(ACTIVITY_FILE),
            clock,
            max_bytes: DEFAULT_MAX_BYTES,
            lock: Mutex::new(()),
        }
    }

    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line: `{"at": <RFC 3339>, "event": …, …}`. Rotates first
    /// when the line would take the file past its limit.
    pub fn record(&self, activity: Activity) -> std::io::Result<()> {
        #[derive(Serialize)]
        struct Line<'a> {
            at: String,
            #[serde(flatten)]
            activity: &'a Activity,
        }
        let mut line = serde_json::to_vec(&Line {
            at: self
                .clock
                .wall()
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            activity: &activity,
        })
        .map_err(std::io::Error::other)?;
        line.push(b'\n');
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size + line.len() as u64 > self.max_bytes {
            let mut rotated = self.path.clone().into_os_string();
            rotated.push(".1");
            std::fs::rename(&self.path, PathBuf::from(rotated))?;
        }
        let mut file = append_private(&self.path)?;
        file.write_all(&line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    fn lines(path: &Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn each_action_is_one_timestamped_line() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new());
        let log = ActivityLog::new(dir.path(), clock.clone());
        let session_id = SessionId::new_v4();
        log.record(Activity::Paired {
            device: "Phone".into(),
        })
        .unwrap();
        log.record(Activity::Prompt {
            device: "Phone".into(),
            session_id,
            chars: 42,
        })
        .unwrap();
        let lines = lines(log.path());
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "paired");
        assert_eq!(lines[0]["device"], "Phone");
        assert_eq!(
            lines[0]["at"]
                .as_str()
                .unwrap()
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap(),
            clock.wall()
        );
        assert_eq!(lines[1]["event"], "prompt");
        assert_eq!(lines[1]["chars"], 42);
        assert_eq!(lines[1]["session_id"], session_id.to_string());
        assert!(
            lines[1].get("text").is_none(),
            "a prompt's text is never logged"
        );
    }

    #[test]
    fn the_log_rotates_instead_of_growing_without_bound() {
        let dir = tempfile::tempdir().unwrap();
        let log = ActivityLog::new(dir.path(), Arc::new(ManualClock::new())).with_max_bytes(400);
        for n in 0..20 {
            log.record(Activity::AuthFailures { count: n }).unwrap();
        }
        let size = std::fs::metadata(log.path()).unwrap().len();
        assert!(size <= 400, "current log is {size} bytes");
        let rotated = dir.path().join(format!("{ACTIVITY_FILE}.1"));
        assert!(rotated.exists(), "the older lines moved aside");
        assert!(std::fs::metadata(&rotated).unwrap().len() <= 400);
        let newest = lines(log.path()).pop().unwrap();
        assert_eq!(newest["count"], 19, "the newest line is kept");
    }

    #[cfg(unix)]
    #[test]
    fn the_log_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = ActivityLog::new(dir.path(), Arc::new(ManualClock::new()));
        log.record(Activity::Disabled).unwrap();
        let mode = std::fs::metadata(log.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
