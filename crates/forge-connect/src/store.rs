//! Credential store — keys + OAuth tokens (connect-command.md §3.4, Phase 6.1).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::OauthTokens;
use crate::profile::KeySource;

/// A credential-store operation failed.
///
/// `Io` and the TOML variants carry their source rather than a flattened string,
/// so a caller can distinguish a missing file from a permissions problem. The
/// `PartialEq` derive is gone because `std::io::Error` does not implement it;
/// nothing compared these values.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml: {0}")]
    TomlDe(#[from] toml::de::Error),
    #[error("toml: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("credentials file permissions too open (expected 0600)")]
    InsecurePermissions,
    /// The file declares a schema newer than this build understands. Refusing is
    /// deliberate: silently mis-reading a newer token layout would look like a
    /// missing credential and prompt the user to re-authenticate needlessly.
    #[error(
        "credentials file declares schema version {found}, but this build understands up to {supported}; upgrade forge to read it"
    )]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
}

/// Highest `credentials.toml` schema version this build can read.
///
/// A file with no `version` key predates versioning and is read as version 1 —
/// the shape those files already have — so existing credentials keep loading.
pub const CREDENTIALS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CredentialsFile {
    /// On-disk schema version. Declared first because TOML requires scalar keys
    /// to precede tables, and `keys`/`oauth` below serialise as tables.
    /// Absent in files written before versioning; `save` always writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<u32>,
    /// profile_id → api key (ApiKey profiles)
    #[serde(default)]
    keys: BTreeMap<String, String>,
    /// profile_id → oauth tokens
    #[serde(default)]
    oauth: BTreeMap<String, OauthTokens>,
    // Interactive selections used to live here too. They are not secrets and
    // they moved to `preferences.rs`; see that module for why. Old files that
    // still carry those keys parse fine — serde ignores them — and shed them
    // on the next write.
}

/// Identity of the on-disk file a cached parse came from.
///
/// Modification time *and* length together: either alone is too easy to repeat
/// across a rewrite. See [`CredentialStore::with_file`] for why a same-tick
/// same-length external rewrite is not a practical risk.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FileStamp {
    Missing,
    Present {
        modified: Option<std::time::SystemTime>,
        len: u64,
    },
}

/// File-backed credential store. Secrets are never returned in status Display APIs.
pub struct CredentialStore {
    path: PathBuf,
    /// Last successful parse, with the file identity it came from.
    ///
    /// Reading credentials sits on the TUI render path (the header's
    /// "connected" state), where it ran several times per frame — each one a
    /// stat, a read, and a full TOML parse of the secrets file. Caching the
    /// parse keeps that to a single stat per call. Only successful loads are
    /// cached, so a permissions or schema error is re-reported every time
    /// rather than latched.
    cache: Mutex<Option<(FileStamp, CredentialsFile)>>,
    /// Reads performed through *this* store, each of which stats the file.
    ///
    /// Per instance rather than process-wide: cargo runs tests in parallel
    /// threads inside one process, so a global counter measures whatever else
    /// happens to be running and cannot be asserted on. Diagnostic only.
    reads: std::sync::atomic::AtomicU64,
}

impl CredentialStore {
    /// Reads performed through this store so far. Each one stats the
    /// credential file, so a caller on a hot path can assert it is not
    /// doing that. Diagnostic only — never a correctness signal.
    pub fn read_count(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
            reads: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn user_default() -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("forge")
            .join("credentials.toml");
        Self::new(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get_api_key(&self, profile_id: &str) -> Result<Option<String>, StoreError> {
        self.with_file(|file| file.keys.get(profile_id).cloned())
    }

    pub fn set_api_key(&self, profile_id: &str, key: &str) -> Result<(), StoreError> {
        let mut file = self.load()?;
        file.keys
            .insert(profile_id.to_string(), key.trim().to_string());
        // Connecting via API key clears oauth for that profile
        file.oauth.remove(profile_id);
        self.save(&file)
    }

    pub fn get_oauth(&self, profile_id: &str) -> Result<Option<OauthTokens>, StoreError> {
        self.with_file(|file| file.oauth.get(profile_id).cloned())
    }

    pub fn set_oauth(&self, profile_id: &str, tokens: OauthTokens) -> Result<(), StoreError> {
        let mut file = self.load()?;
        file.oauth.insert(profile_id.to_string(), tokens);
        // OAuth supersedes stored API key for this profile
        file.keys.remove(profile_id);
        self.save(&file)
    }

    pub fn clear(&self, profile_id: &str) -> Result<bool, StoreError> {
        let mut file = self.load()?;
        let removed_key = file.keys.remove(profile_id).is_some();
        let removed_oauth = file.oauth.remove(profile_id).is_some();
        let removed = removed_key || removed_oauth;
        if removed {
            self.save(&file)?;
        }
        Ok(removed)
    }

    pub fn clear_all(&self) -> Result<bool, StoreError> {
        let mut file = self.load()?;
        let removed = !file.keys.is_empty() || !file.oauth.is_empty();
        if removed {
            file.keys.clear();
            file.oauth.clear();
            self.save(&file)?;
        }
        Ok(removed)
    }

    pub fn is_connected(&self, profile_id: &str) -> Result<bool, StoreError> {
        self.with_file(|file| {
            file.keys.contains_key(profile_id) || file.oauth.contains_key(profile_id)
        })
    }

    pub fn list_profile_ids(&self) -> Result<Vec<String>, StoreError> {
        self.with_file(|file| {
            let mut ids: Vec<String> = file.keys.keys().cloned().collect();
            for k in file.oauth.keys() {
                if !ids.iter().any(|i| i == k) {
                    ids.push(k.clone());
                }
            }
            ids.sort();
            ids
        })
    }

    /// Read the parsed credentials file, reusing the cached parse when the file
    /// on disk is unchanged.
    ///
    /// The closure runs while the cache lock is held, so it must not call back
    /// into this store — every caller here is a map lookup or a field read.
    ///
    /// Freshness is decided by modification time plus length. An external
    /// rewrite that lands in the same filesystem timestamp tick *and* keeps the
    /// byte length identical would be missed; writes made through this store
    /// refresh the cache directly, so that needs a second process racing inside
    /// one tick, which no credential flow does.
    fn with_file<T>(&self, read: impl FnOnce(&CredentialsFile) -> T) -> Result<T, StoreError> {
        // Every call stats the file (see the permissions gate below), so this
        // counter is how a caller can assert it is not doing that per frame.
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = match fs::metadata(&self.path) {
            Ok(meta) => {
                // The permissions gate runs on every read, never from cache: a
                // chmod changes ctime only, so a file that just became
                // world-readable is byte-identical and would otherwise sail
                // through as a cache hit. It reuses the metadata already read
                // here, so enforcing it every time costs no extra syscall.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o077 != 0 {
                        return Err(StoreError::InsecurePermissions);
                    }
                }
                FileStamp::Present {
                    modified: meta.modified().ok(),
                    len: meta.len(),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileStamp::Missing,
            Err(error) => return Err(error.into()),
        };

        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some((cached_stamp, file)) = cache.as_ref() {
            if *cached_stamp == stamp {
                return Ok(read(file));
            }
        }

        let file = self.read_uncached(stamp)?;
        let value = read(&file);
        *cache = Some((stamp, file));
        Ok(value)
    }

    /// Parse the file from disk, bypassing the cache.
    fn read_uncached(&self, stamp: FileStamp) -> Result<CredentialsFile, StoreError> {
        if stamp == FileStamp::Missing {
            return Ok(CredentialsFile::default());
        }
        // Permissions are already verified by `with_file`, the sole caller.
        let text = fs::read_to_string(&self.path)?;
        let file: CredentialsFile = toml::from_str(&text)?;
        // A file with no `version` predates versioning and is read as v1.
        if let Some(found) = file.version {
            if found > CREDENTIALS_SCHEMA_VERSION {
                return Err(StoreError::UnsupportedSchemaVersion {
                    found,
                    supported: CREDENTIALS_SCHEMA_VERSION,
                });
            }
        }
        Ok(file)
    }

    /// An owned copy of the parsed file, for the read-modify-write paths.
    fn load(&self) -> Result<CredentialsFile, StoreError> {
        self.with_file(Clone::clone)
    }

    fn save(&self, file: &CredentialsFile) -> Result<(), StoreError> {
        // Stamp the current version on every write so a future build can tell
        // what layout it is reading instead of guessing.
        let mut file = file.clone();
        file.version = Some(CREDENTIALS_SCHEMA_VERSION);
        let file = &file;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(file)?;
        fs::write(&self.path, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = fs::Permissions::from_mode(0o600);
            fs::set_permissions(&self.path, perms)?;
        }
        // Re-stamp from the file just written. Dropping the entry instead would
        // also be correct, but this keeps the next read a cache hit, and it
        // closes the window where a write landing in the same timestamp tick as
        // the cached parse would leave the stale copy looking fresh.
        let stamp = match fs::metadata(&self.path) {
            Ok(meta) => FileStamp::Present {
                modified: meta.modified().ok(),
                len: meta.len(),
            },
            Err(_) => FileStamp::Missing,
        };
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *cache = match stamp {
            FileStamp::Missing => None,
            stamp => Some((stamp, file.clone())),
        };
        Ok(())
    }
}

/// Resolve API key: env then file (ApiKey profiles).
pub fn resolve_key(
    profile_env_names: &[String],
    profile_id: &str,
    store: &CredentialStore,
) -> Result<Option<(String, KeySource)>, StoreError> {
    for name in profile_env_names {
        if let Ok(v) = std::env::var(name) {
            if !v.trim().is_empty() {
                return Ok(Some((v, KeySource::Env)));
            }
        }
    }
    if let Some(k) = store.get_api_key(profile_id)? {
        if !k.trim().is_empty() {
            return Ok(Some((k, KeySource::File)));
        }
    }
    Ok(None)
}

/// Whether profile has usable credentials (API key path or OAuth tokens).
pub fn resolve_connected(
    profile_env_names: &[String],
    profile_id: &str,
    store: &CredentialStore,
) -> Result<Option<KeySource>, StoreError> {
    let (has_oauth, has_key) = store.with_file(|file| {
        (
            file.oauth.contains_key(profile_id),
            file.keys
                .get(profile_id)
                .is_some_and(|key| !key.trim().is_empty()),
        )
    })?;
    if has_oauth {
        return Ok(Some(KeySource::Oauth));
    }
    for name in profile_env_names {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Ok(Some(KeySource::Env));
            }
        }
    }
    Ok(has_key.then_some(KeySource::File))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn set_get_clear_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("credentials.toml"));
        assert!(store.get_api_key("xai").unwrap().is_none());
        store.set_api_key("xai", "secret-key").unwrap();
        assert_eq!(
            store.get_api_key("xai").unwrap().as_deref(),
            Some("secret-key")
        );
        assert!(store.clear("xai").unwrap());
        assert!(store.get_api_key("xai").unwrap().is_none());
    }

    #[test]
    fn oauth_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store
            .set_oauth(
                "xai",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: Some("rt".into()),
                    expires_at: None,
                },
            )
            .unwrap();
        let t = store.get_oauth("xai").unwrap().unwrap();
        assert_eq!(t.access_token, "at");
        assert_eq!(t.refresh_token.as_deref(), Some("rt"));
        assert!(store.is_connected("xai").unwrap());
        assert_eq!(
            resolve_connected(&[], "xai", &store).unwrap(),
            Some(KeySource::Oauth)
        );
    }

    #[test]
    fn clear_all_roundtrip() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "secret").unwrap();
        store
            .set_oauth(
                "openai_codex",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        assert!(store.clear_all().unwrap());
        assert!(store.get_api_key("xai").unwrap().is_none());
        assert!(store.get_oauth("openai_codex").unwrap().is_none());
    }

    #[test]
    fn file_mode_is_private_on_unix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = CredentialStore::new(path.clone());
        store.set_api_key("xai", "k").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn resolve_prefers_env() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "from-file").unwrap();
        let guard = crate::test_env::EnvGuard::new(&["FORGE_TEST_XAI_KEY"]);
        guard.set("FORGE_TEST_XAI_KEY", "from-env");
        let r = resolve_key(&["FORGE_TEST_XAI_KEY".into()], "xai", &store)
            .unwrap()
            .unwrap();
        assert_eq!(r.0, "from-env");
        assert_eq!(r.1, KeySource::Env);
    }

    /// The regression that matters most here: an existing `credentials.toml` has
    /// no `version` key, and it must keep loading. A failure locks users out of
    /// stored tokens and looks like a missing credential.
    #[test]
    fn credentials_without_a_version_key_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        std::fs::write(&path, "[keys]\nopenai = \"sk-existing\"\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = CredentialStore::new(path);
        assert_eq!(
            store.get_api_key("openai").unwrap(),
            Some("sk-existing".to_string()),
            "an unversioned credentials file must keep loading"
        );
    }

    /// A newer file is refused rather than mis-read. Silently failing to find a
    /// token would prompt a needless re-authentication.
    #[test]
    fn credentials_from_a_newer_build_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let future = CREDENTIALS_SCHEMA_VERSION + 1;
        std::fs::write(
            &path,
            format!("version = {future}\n[keys]\nopenai = \"x\"\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = CredentialStore::new(path);
        let err = store.get_api_key("openai").unwrap_err();
        // Matched on the variant rather than compared for equality, so this test
        // does not depend on StoreError deriving PartialEq.
        assert!(
            matches!(
                err,
                StoreError::UnsupportedSchemaVersion { found: f, supported }
                    if f == future && supported == CREDENTIALS_SCHEMA_VERSION
            ),
            "expected an unsupported-version error, got {err:?}"
        );
    }

    /// Writing stamps the version, and it round-trips alongside a populated
    /// `[keys]` table — which also pins that the scalar is emitted before the
    /// table, as TOML requires.
    #[test]
    fn saving_stamps_the_version_and_round_trips_with_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        let store = CredentialStore::new(path.clone());
        store.set_api_key("openai", "sk-written").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains(&format!("version = {CREDENTIALS_SCHEMA_VERSION}")),
            "save must stamp the schema version, got:\n{text}"
        );
        assert!(
            text.find("version").unwrap() < text.find("[keys]").unwrap(),
            "the version scalar must precede the [keys] table, got:\n{text}"
        );
        assert_eq!(
            store.get_api_key("openai").unwrap(),
            Some("sk-written".to_string())
        );
    }

    #[test]
    fn path_reports_the_backing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("credentials.toml");
        let store = CredentialStore::new(path.clone());
        assert_eq!(store.path(), path.as_path());
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        // Nothing has created `a/b` yet; writing a credential must not fail.
        let store = CredentialStore::new(dir.path().join("a").join("b").join("c.toml"));
        store.set_api_key("xai", "k").unwrap();
        assert!(store.path().exists());
        assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("k"));
    }

    #[test]
    fn clear_reports_false_when_there_was_nothing_to_remove() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert!(!store.clear("absent").unwrap());
    }

    #[test]
    fn clear_all_removes_every_kind_of_stored_state() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));

        // Nothing stored yet, so there is nothing to clear.
        assert!(!store.clear_all().unwrap());

        store.set_api_key("xai", "k").unwrap();
        store
            .set_oauth(
                "openai",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();

        assert!(store.clear_all().unwrap());

        assert!(store.get_api_key("xai").unwrap().is_none());
        assert!(store.get_oauth("openai").unwrap().is_none());
        assert!(store.list_profile_ids().unwrap().is_empty());
        // Idempotent: a second call has nothing left to do.
        assert!(!store.clear_all().unwrap());
    }

    #[test]
    fn list_profile_ids_merges_both_credential_kinds_sorted_and_deduped() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert!(store.list_profile_ids().unwrap().is_empty());

        store.set_api_key("zeta", "k").unwrap();
        store.set_api_key("alpha", "k").unwrap();
        store
            .set_oauth(
                "middle",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();
        // `alpha` holds both an API key and OAuth tokens; it must appear once.
        store
            .set_oauth(
                "alpha",
                OauthTokens {
                    access_token: "at".into(),
                    refresh_token: None,
                    expires_at: None,
                },
            )
            .unwrap();

        assert_eq!(
            store.list_profile_ids().unwrap(),
            vec![
                "alpha".to_string(),
                "middle".to_string(),
                "zeta".to_string()
            ]
        );
    }

    #[test]
    fn resolve_key_prefers_the_environment_then_falls_back_to_the_file() {
        const KEY: &str = "FORGE_TEST_STORE_RESOLVE_KEY";
        let guard = crate::test_env::EnvGuard::new(&[KEY]);
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        let names = vec![KEY.to_string()];

        // Neither source has anything.
        assert_eq!(resolve_key(&names, "xai", &store).unwrap(), None);

        // File only.
        store.set_api_key("xai", "from-file").unwrap();
        assert_eq!(
            resolve_key(&names, "xai", &store).unwrap(),
            Some(("from-file".to_string(), KeySource::File))
        );

        // The environment wins when both are present.
        guard.set(KEY, "from-env");
        assert_eq!(
            resolve_key(&names, "xai", &store).unwrap(),
            Some(("from-env".to_string(), KeySource::Env))
        );

        // A blank environment value is ignored rather than masking the file.
        guard.set(KEY, "   ");
        assert_eq!(
            resolve_key(&names, "xai", &store).unwrap(),
            Some(("from-file".to_string(), KeySource::File))
        );
    }

    #[cfg(unix)]
    #[test]
    fn loading_a_world_readable_credentials_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "secret").unwrap();

        // 0600 is the expected mode and must still load.
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("secret"));

        // Any group or other bit set means the secret is readable by another
        // account, so the store refuses rather than silently continuing.
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = store.get_api_key("xai").unwrap_err();
        assert!(
            matches!(err, StoreError::InsecurePermissions),
            "expected InsecurePermissions, got {err:?}"
        );
        assert!(!err.to_string().contains("secret"));
    }

    /// A rewrite by another process must be observed by the next read.
    ///
    /// Reads are served from a cached parse keyed on the file's modification
    /// time and length, so the risk this caching introduces is a stale answer.
    /// A credential edited or revoked out-of-band has to take effect.
    #[test]
    fn an_external_rewrite_invalidates_the_cached_parse() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "first").unwrap();
        assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("first"));

        // Rewrite behind the store's back, the way a second process would.
        let replacement = CredentialStore::new(store.path().to_path_buf());
        replacement.set_api_key("xai", "second-value").unwrap();

        assert_eq!(
            store.get_api_key("xai").unwrap().as_deref(),
            Some("second-value"),
            "a cached parse must not outlive the file it came from"
        );
    }

    /// Clearing a credential elsewhere must not leave the old one readable.
    #[test]
    fn an_external_clear_invalidates_the_cached_parse() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "secret").unwrap();
        assert!(store.is_connected("xai").unwrap());

        CredentialStore::new(store.path().to_path_buf())
            .clear("xai")
            .unwrap();

        assert!(
            !store.is_connected("xai").unwrap(),
            "a revoked credential must not stay live in the cache"
        );
    }

    /// Repeated reads of an unchanged file agree with each other.
    #[test]
    fn repeated_reads_of_an_unchanged_file_are_stable() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        store.set_api_key("xai", "secret").unwrap();

        for _ in 0..3 {
            assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("secret"));
        }
    }

    /// A store pointed at a path that does not exist reads as empty, and starts
    /// serving real values once the file appears.
    #[test]
    fn a_missing_file_reads_empty_and_notices_when_it_appears() {
        let dir = tempdir().unwrap();
        let store = CredentialStore::new(dir.path().join("c.toml"));
        assert_eq!(store.get_api_key("xai").unwrap(), None);

        CredentialStore::new(store.path().to_path_buf())
            .set_api_key("xai", "secret")
            .unwrap();

        assert_eq!(store.get_api_key("xai").unwrap().as_deref(), Some("secret"));
    }
}
