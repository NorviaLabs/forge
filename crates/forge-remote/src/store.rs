//! The paired device: at most one, remembered across restarts.
//!
//! `devices.json` holds the device's id, name, dates and the SHA-256 of its
//! token — never the token. Pairing a new device replaces the old record in
//! one atomic write; revoking deletes it. A file that cannot be read or
//! parsed means no device (fail closed), and the reason is kept for the
//! desktop to show.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::fsutil::write_private;
use crate::token::{DeviceToken, TokenHash};

pub const DEVICES_FILE: &str = "devices.json";
/// `last_seen` reaches the disk at most this often; in memory it is exact.
pub const LAST_SEEN_WRITE_INTERVAL: Duration = Duration::from_secs(60);
/// The longest device name, in characters.
pub const MAX_DEVICE_NAME: usize = 64;

/// The paired device, as the desktop shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub paired_at: DateTime<Utc>,
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot save the paired device: {0}")]
    Io(#[from] std::io::Error),
}

/// What `devices.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    device: Device,
    /// Hex SHA-256 of the token.
    token_sha256: String,
}

struct State {
    record: Option<Record>,
    /// When `last_seen` last reached the disk.
    flushed: Option<Instant>,
    dirty: bool,
}

pub struct DeviceStore {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    load_error: Option<String>,
}

impl DeviceStore {
    /// Read `dir/devices.json`. Never fails: an unreadable or malformed file
    /// is no device, with [`Self::load_error`] saying why.
    pub fn open(dir: &Path, clock: Arc<dyn Clock>) -> Self {
        let path = dir.join(DEVICES_FILE);
        let (record, load_error) = match std::fs::read(&path) {
            Ok(bytes) => match parse_record(&bytes) {
                Ok(record) => (Some(record), None),
                Err(why) => (
                    None,
                    Some(format!(
                        "the paired device record is damaged ({why}); pair again"
                    )),
                ),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
            Err(error) => (
                None,
                Some(format!("cannot read the paired device record: {error}")),
            ),
        };
        Self {
            path,
            clock,
            state: Mutex::new(State {
                record,
                flushed: None,
                dirty: false,
            }),
            load_error,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn load_error(&self) -> Option<String> {
        self.load_error.clone()
    }

    pub fn device(&self) -> Option<Device> {
        self.state().record.as_ref().map(|r| r.device.clone())
    }

    /// Pair a device named `name` (already cleaned), replacing any other.
    /// Returns the new token: the only time it exists outside the device.
    pub fn pair(&self, name: &str) -> Result<(Device, DeviceToken), StoreError> {
        let token = DeviceToken::generate();
        let device = Device {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            paired_at: self.clock.wall(),
            last_seen: None,
        };
        let record = Record {
            device: device.clone(),
            token_sha256: TokenHash::of(&token).to_hex(),
        };
        let mut state = self.state();
        self.write(Some(&record))?;
        state.record = Some(record);
        state.flushed = Some(self.clock.now());
        state.dirty = false;
        Ok((device, token))
    }

    /// Forget the device. Its token stops working at once.
    pub fn revoke(&self) -> Result<Option<Device>, StoreError> {
        let mut state = self.state();
        let Some(record) = state.record.take() else {
            return Ok(None);
        };
        if let Err(error) = self.write(None) {
            state.record = Some(record);
            return Err(error);
        }
        state.dirty = false;
        Ok(Some(record.device))
    }

    /// The device `presented` belongs to, if any. Records `last_seen`.
    pub fn authenticate(&self, presented: &str) -> Option<Device> {
        let token = DeviceToken::parse(presented)?;
        let mut state = self.state();
        let record = state.record.as_mut()?;
        let hash = TokenHash::from_hex(&record.token_sha256)?;
        if !hash.matches(&token) {
            return None;
        }
        // The first sighting is always written; later ones at most once
        // per interval.
        let first = record.device.last_seen.is_none();
        record.device.last_seen = Some(self.clock.wall());
        let device = record.device.clone();
        state.dirty = true;
        let due = first
            || state.flushed.is_none_or(|at| {
                self.clock.now().saturating_duration_since(at) >= LAST_SEEN_WRITE_INTERVAL
            });
        if due {
            // Best effort: a failed write leaves it dirty for the next try.
            let _ = self.flush_locked(&mut state);
        }
        Some(device)
    }

    /// Write a `last_seen` that is waiting for its interval to pass.
    pub fn flush(&self) -> Result<(), StoreError> {
        let mut state = self.state();
        self.flush_locked(&mut state)
    }

    fn flush_locked(&self, state: &mut State) -> Result<(), StoreError> {
        if !state.dirty {
            return Ok(());
        }
        if let Some(record) = &state.record {
            self.write(Some(record))?;
        }
        state.dirty = false;
        state.flushed = Some(self.clock.now());
        Ok(())
    }

    fn write(&self, record: Option<&Record>) -> Result<(), StoreError> {
        match record {
            Some(record) => {
                let bytes = serde_json::to_vec_pretty(record)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e)))?;
                write_private(&self.path, &bytes)?;
            }
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            },
        }
        Ok(())
    }
}

fn parse_record(bytes: &[u8]) -> Result<Record, String> {
    let record: Record = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if TokenHash::from_hex(&record.token_sha256).is_none() {
        return Err("no valid token hash".into());
    }
    Ok(record)
}

/// A device name fit to show: trimmed, 1 to [`MAX_DEVICE_NAME`] characters,
/// no control characters, invisible and bidi characters made visible.
pub fn clean_device_name(input: &str) -> Option<String> {
    let name = input.trim();
    if name.is_empty()
        || name.chars().count() > MAX_DEVICE_NAME
        || name.chars().any(char::is_control)
    {
        return None;
    }
    Some(forge_session::approvals::escape_invisible(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use base64::Engine;

    fn store() -> (tempfile::TempDir, Arc<ManualClock>, DeviceStore) {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new());
        let store = DeviceStore::open(dir.path(), clock.clone());
        (dir, clock, store)
    }

    fn files_text(dir: &Path) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file() {
                text.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
            }
        }
        text
    }

    #[test]
    fn nothing_is_paired_at_first() {
        let (_dir, _, store) = store();
        assert_eq!(store.device(), None);
        assert_eq!(store.load_error(), None);
        assert_eq!(store.authenticate("fr1_anything"), None);
    }

    #[test]
    fn a_paired_device_authenticates_with_its_token_only() {
        let (_dir, _, store) = store();
        let (device, token) = store.pair("Priya's iPhone").unwrap();
        assert_eq!(device.name, "Priya's iPhone");
        assert_eq!(store.device().map(|d| d.id), Some(device.id.clone()));
        assert_eq!(
            store.authenticate(token.as_str()).map(|d| d.id),
            Some(device.id)
        );
        assert_eq!(store.authenticate(DeviceToken::generate().as_str()), None);
        assert_eq!(store.authenticate(""), None);
        assert_eq!(store.authenticate("not a token"), None);
    }

    #[test]
    fn the_token_is_never_written_only_its_hash() {
        let (dir, _, store) = store();
        let (_, token) = store.pair("Phone").unwrap();
        store.authenticate(token.as_str()).unwrap();
        store.flush().unwrap();
        let secret = &token.as_str()[crate::token::TOKEN_PREFIX.len()..];
        let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(secret)
            .unwrap();
        let text = files_text(dir.path());
        for form in [
            token.as_str().to_string(),
            secret.to_string(),
            hex::encode(&raw),
            hex::encode(&raw).to_uppercase(),
            base64::engine::general_purpose::STANDARD.encode(&raw),
            base64::engine::general_purpose::URL_SAFE.encode(&raw),
        ] {
            assert!(!text.contains(&form), "the token leaked to disk as {form}");
        }
        let record: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(DEVICES_FILE)).unwrap())
                .unwrap();
        assert_eq!(record["token_sha256"], TokenHash::of(&token).to_hex());
    }

    #[test]
    fn pairing_survives_a_restart() {
        let (dir, clock, store) = store();
        let (device, token) = store.pair("Phone").unwrap();
        drop(store);
        let reopened = DeviceStore::open(dir.path(), clock);
        assert_eq!(reopened.device().map(|d| d.id), Some(device.id));
        assert!(reopened.authenticate(token.as_str()).is_some());
    }

    #[test]
    fn pairing_again_replaces_the_device_and_its_token() {
        let (dir, clock, store) = store();
        let (first, old) = store.pair("Old phone").unwrap();
        let (second, new) = store.pair("New phone").unwrap();
        assert_ne!(first.id, second.id);
        assert_eq!(
            store.authenticate(old.as_str()),
            None,
            "the old token is dead"
        );
        assert!(store.authenticate(new.as_str()).is_some());
        let reopened = DeviceStore::open(dir.path(), clock);
        assert_eq!(reopened.authenticate(old.as_str()), None);
        assert_eq!(reopened.device().map(|d| d.name), Some("New phone".into()));
    }

    #[test]
    fn a_revoked_token_stops_working_now_and_after_a_restart() {
        let (dir, clock, store) = store();
        let (device, token) = store.pair("Phone").unwrap();
        assert_eq!(store.revoke().unwrap().map(|d| d.id), Some(device.id));
        assert_eq!(store.authenticate(token.as_str()), None);
        assert_eq!(store.device(), None);
        assert_eq!(store.revoke().unwrap(), None);
        let reopened = DeviceStore::open(dir.path(), clock);
        assert_eq!(reopened.authenticate(token.as_str()), None);
    }

    #[test]
    fn a_damaged_record_means_no_device() {
        let (dir, clock, store) = store();
        let (_, token) = store.pair("Phone").unwrap();
        std::fs::write(dir.path().join(DEVICES_FILE), "{ not json").unwrap();
        let reopened = DeviceStore::open(dir.path(), clock.clone());
        assert_eq!(reopened.device(), None);
        assert_eq!(reopened.authenticate(token.as_str()), None);
        assert!(reopened.load_error().is_some());

        std::fs::write(
            dir.path().join(DEVICES_FILE),
            r#"{"id":"d","name":"n","paired_at":"2026-01-01T00:00:00Z","last_seen":null,"token_sha256":"short"}"#,
        )
        .unwrap();
        let reopened = DeviceStore::open(dir.path(), clock);
        assert_eq!(reopened.device(), None, "a malformed hash is no device");
        assert!(reopened.load_error().is_some());
    }

    #[test]
    fn last_seen_is_exact_in_memory_and_written_at_most_once_a_minute() {
        let (dir, clock, store) = store();
        let (_, token) = store.pair("Phone").unwrap();
        let on_disk = || -> Option<DateTime<Utc>> {
            let value: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(dir.path().join(DEVICES_FILE)).unwrap(),
            )
            .unwrap();
            value["last_seen"]
                .as_str()
                .map(|text| text.parse::<DateTime<Utc>>().unwrap())
        };
        let first = store.authenticate(token.as_str()).unwrap();
        assert_eq!(first.last_seen, Some(clock.wall()));
        let written = on_disk().expect("the first sighting is written");
        assert_eq!(written, clock.wall());

        clock.advance(Duration::from_secs(10));
        let second = store.authenticate(token.as_str()).unwrap();
        assert_eq!(second.last_seen, Some(clock.wall()), "memory is exact");
        assert_eq!(
            on_disk(),
            Some(written),
            "not written again within a minute"
        );

        clock.advance(LAST_SEEN_WRITE_INTERVAL);
        store.authenticate(token.as_str()).unwrap();
        assert_eq!(
            on_disk(),
            Some(clock.wall()),
            "written once a minute has passed"
        );

        clock.advance(Duration::from_secs(5));
        store.authenticate(token.as_str()).unwrap();
        store.flush().unwrap();
        assert_eq!(
            on_disk(),
            Some(clock.wall()),
            "flush writes the pending sighting"
        );
        let reopened = DeviceStore::open(dir.path(), clock.clone());
        assert_eq!(reopened.device().unwrap().last_seen, Some(clock.wall()));
    }

    #[cfg(unix)]
    #[test]
    fn the_record_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _, store) = store();
        store.pair("Phone").unwrap();
        let mode = std::fs::metadata(dir.path().join(DEVICES_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn device_names_are_cleaned() {
        assert_eq!(clean_device_name("  Phone  ").as_deref(), Some("Phone"));
        assert_eq!(clean_device_name(""), None);
        assert_eq!(clean_device_name("   "), None);
        assert_eq!(clean_device_name("line\nbreak"), None);
        assert_eq!(clean_device_name("bell\u{7}"), None);
        assert_eq!(
            clean_device_name("Phone\u{202E}enohP").as_deref(),
            Some("Phone⟨U+202E⟩enohP")
        );
        assert!(clean_device_name(&"x".repeat(MAX_DEVICE_NAME)).is_some());
        assert_eq!(clean_device_name(&"x".repeat(MAX_DEVICE_NAME + 1)), None);
        assert_eq!(
            clean_device_name(&"é".repeat(MAX_DEVICE_NAME)).map(|n| n.chars().count()),
            Some(MAX_DEVICE_NAME),
            "the limit counts characters, not bytes"
        );
    }
}
